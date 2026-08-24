// iLink Cloudflare 前置密码门 Worker —— 3XUI 隐藏路径模式
// ─────────────────────────────────────────────────────────────
// 架构：
//   - Worker 接管 ilink.354199.xyz/* 全部（catch-all）
//   - 除部署上报端点外，所有路径均须通过密码门 session 验证
//   - secret 路径（/<6-8位短码>/<...>）：3XUI 隐藏路径，fetch 反代到 ilink-wm1
//
// TUNNEL_ORIGIN 必须是独立的 Tunnel 回源域名（例如 origin.example.com），
//   且它不能绑定本 Worker Route；避免同域名 subrequest 形成循环。
//
// 短码：
//   - 6-8 位 [a-z0-9] 随机，存到 KV `gate_secret`
//   - 启动时若 KV 无则自动生成
//   - 验证密码后展示两个选项：过 CF (带短码) / IPv6 直连
// ─────────────────────────────────────────────────────────────

const CHAT_PATH_DEFAULT = '/chat';
const RATE_MAX = 5;
const RATE_WINDOW_SEC = 300;
const KV_KEY_IPV6 = 'latest_ipv6';
const KV_KEY_LASTSEEN = 'last_seen';
const KV_KEY_SECRET = 'gate_secret';
const SESSION_COOKIE = '__Host-ilink_gate';
const SESSION_PREFIX = 'gate_session:';
const SESSION_TTL_SEC = 24 * 60 * 60;

// 短码：6-8 位 [a-z0-9]
function randomSecret(len = 7) {
  const chars = 'abcdefghijklmnopqrstuvwxyz0123456789';
  let s = '';
  const buf = new Uint8Array(len);
  crypto.getRandomValues(buf);
  for (let i = 0; i < len; i++) s += chars[buf[i] % chars.length];
  return s;
}
async function getOrCreateSecret(kv) {
  let s = await kv.get(KV_KEY_SECRET);
  if (!s || !/^[a-z0-9]{6,8}$/.test(s)) {
    s = randomSecret(7);
    await kv.put(KV_KEY_SECRET, s);
  }
  return s;
}

function cookieValue(request, name) {
  const cookies = request.headers.get('Cookie') || '';
  const pair = cookies.split(';').map(v => v.trim()).find(v => v.startsWith(name + '='));
  return pair ? pair.slice(name.length + 1) : '';
}
async function hasGateSession(request, kv, ip) {
  const token = cookieValue(request, SESSION_COOKIE);
  if (!/^[a-z0-9]{48}$/.test(token)) return false;
  const savedIp = await kv.get(SESSION_PREFIX + token);
  return savedIp === ip;
}
async function createGateSession(kv, ip) {
  const token = randomSecret(48);
  await kv.put(SESSION_PREFIX + token, ip, { expirationTtl: SESSION_TTL_SEC });
  return `${SESSION_COOKIE}=${token}; Path=/; Max-Age=${SESSION_TTL_SEC}; Secure; HttpOnly; SameSite=Strict`;
}
function gateRequired(url) {
  if (url.pathname.startsWith('/api/') || url.pathname.startsWith('/static/')) {
    return json({ error: 'gate_session_required' }, 401);
  }
  return new Response('访问验证已过期，请从根路径重新验证。', { status: 401, headers: { 'Cache-Control': 'no-store' } });
}

// 密码页 HTML
function htmlShell({ ipv6Url, cfUrl, hasIpv6, secret, nextPath }) {
  const safe = (s) => String(s).replace(/[&<>"']/g, c => ({
    '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;', "'": '&#39;'
  }[c]));

  // 验证成功后，给两个选项：
  //   1) 过 CF：URL 是 https://chat.example.com/<secret>/chat
  //   2) IPv6 直连：URL 是 https://direct.example.com/chat（DNS-only AAAA + HTTPS）
  const cfUrlWithSecret = secret && cfUrl ? `https://${new URL(cfUrl).host}/${secret}${new URL(cfUrl).pathname}` : cfUrl;

  return `<!DOCTYPE html>
<html lang="zh-CN">
<head>
<meta charset="UTF-8">
<meta name="viewport" content="width=device-width, initial-scale=1.0">
<meta name="theme-color" content="#0a0a0a">
<title>访问验证</title>
<style>
:root {
  --bg-primary:#0a0a0a; --bg-secondary:#111; --bg-tertiary:#1a1a1a;
  --border:#222; --text:#f5f5f7; --text-2:#a1a1a6; --text-3:#6e6e73;
  --accent:#34C759; --accent-h:#4dd875; --cf:#F38020;
  --r-md:12px; --r-lg:16px; --font:-apple-system,BlinkMacSystemFont,"Segoe UI","Noto Sans SC","PingFang SC","Microsoft YaHei",sans-serif;
}
*{margin:0;padding:0;box-sizing:border-box}
body{font-family:var(--font);background:var(--bg-primary);color:var(--text);min-height:100vh;display:flex;align-items:center;justify-content:center;padding:20px;-webkit-font-smoothing:antialiased}
.card{width:100%;max-width:480px;background:var(--bg-secondary);border:1px solid var(--border);border-radius:var(--r-lg);padding:36px 32px;box-shadow:0 4px 20px rgba(0,0,0,.4)}
.badge{display:inline-flex;align-items:center;gap:6px;padding:6px 14px;background:rgba(52,199,89,.08);border:1px solid rgba(52,199,89,.15);border-radius:20px;font-size:12px;color:var(--accent);margin-bottom:24px}
.badge.cf{background:rgba(243,128,32,.10);border-color:rgba(243,128,32,.25);color:var(--cf)}
h1{font-size:24px;font-weight:700;letter-spacing:-.02em;margin-bottom:8px}
.subtitle{font-size:13px;color:var(--text-2);margin-bottom:24px;line-height:1.5}
.input-group{position:relative;margin-bottom:14px}
input[type=password]{width:100%;padding:13px 16px;font-size:15px;font-family:var(--font);background:var(--bg-tertiary);border:1px solid var(--border);border-radius:var(--r-md);color:var(--text);outline:none;transition:border-color .15s}
input[type=password]:focus{border-color:var(--accent)}
input[type=password]::placeholder{color:var(--text-3)}
button.primary{width:100%;padding:13px;font-size:15px;font-weight:600;font-family:var(--font);background:var(--accent);color:#000;border:0;border-radius:var(--r-md);cursor:pointer;transition:all .15s}
button.primary:hover{background:var(--accent-h);transform:translateY(-1px);box-shadow:0 4px 16px rgba(52,199,89,.3)}
button.primary:active{transform:translateY(0)}
button.primary:disabled{opacity:.5;cursor:not-allowed;transform:none}
.error{background:rgba(255,59,48,.1);border:1px solid rgba(255,59,48,.2);color:#FF3B30;padding:12px 16px;border-radius:var(--r-md);font-size:13px;margin-bottom:16px;display:none}
.error.show{display:block;animation:shake .3s ease}
@keyframes shake{0%,100%{transform:translateX(0)}25%{transform:translateX(-4px)}75%{transform:translateX(4px)}}
.access-list{display:flex;flex-direction:column;gap:14px;margin-top:4px}
.access-card{background:var(--bg-tertiary);border:1px solid var(--border);border-radius:var(--r-md);padding:14px 16px;display:flex;flex-direction:column;gap:10px;transition:border-color .15s,transform .15s}
.access-card:hover{border-color:#3a3a3a;transform:translateY(-1px)}
.access-card.disabled{opacity:.45}
.access-card.disabled .access-btn{pointer-events:none}
.access-head{display:flex;align-items:center;gap:10px;flex-wrap:wrap}
.access-title{font-size:15px;font-weight:600;color:var(--text)}
.access-tag{font-size:11px;padding:2px 8px;border-radius:10px;background:rgba(255,255,255,.04);color:var(--text-2);border:1px solid var(--border)}
.access-tag.cf{background:rgba(243,128,32,.10);color:var(--cf);border-color:rgba(243,128,32,.25)}
.access-tag.secret{background:rgba(52,199,89,.10);color:var(--accent);border-color:rgba(52,199,89,.25)}
.access-url{font-family:ui-monospace,SFMono-Regular,Consolas,monospace;font-size:12px;color:var(--text-2);word-break:break-all;background:rgba(255,255,255,.02);padding:8px 10px;border-radius:8px;border:1px solid var(--border);user-select:all;line-height:1.4}
.access-actions{display:flex;gap:8px}
.access-btn{flex:1;padding:9px 12px;font-size:13px;font-weight:600;font-family:var(--font);border:1px solid var(--border);border-radius:8px;cursor:pointer;background:var(--bg-secondary);color:var(--text);transition:all .15s}
.access-btn.primary{background:var(--accent);color:#000;border-color:var(--accent)}
.access-btn.primary.cf{background:var(--cf);border-color:var(--cf);color:#000}
.access-btn:hover{transform:translateY(-1px)}
.access-hint{font-size:11.5px;color:var(--text-3);line-height:1.5}
.footer{text-align:center;margin-top:18px;font-size:12px;color:var(--text-3)}
.hidden{display:none!important}
.toast{position:fixed;left:50%;bottom:32px;transform:translateX(-50%);background:rgba(52,199,89,.95);color:#000;padding:10px 18px;border-radius:22px;font-size:13px;font-weight:600;box-shadow:0 6px 20px rgba(0,0,0,.4);opacity:0;transition:opacity .2s,transform .2s;pointer-events:none;z-index:10}
.toast.show{opacity:1;transform:translate(-50%,-4px)}
@media (max-width:480px){.card{padding:24px 18px}}
</style>
</head>
<body>
<div class="card">
  <div id="view-auth">
    <div class="badge">
      <svg width="12" height="12" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><rect x="3" y="11" width="18" height="11" rx="2"/><path d="M7 11V7a5 5 0 0110 0v4"/></svg>
      安全访问
    </div>
    <h1>访问验证</h1>
    <p class="subtitle">请输入访问密码以继续</p>
    <div class="error" id="error"></div>
    <form id="form">
      <div class="input-group"><input type="password" id="password" placeholder="密码" required autocomplete="current-password"></div>
      <button type="submit" id="submit" class="primary">验证</button>
    </form>
  </div>

  <div id="view-choose" class="hidden">
    <div class="badge"><svg width="12" height="12" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.2"><path d="M20 6L9 17l-5-5"/></svg>验证成功</div>
    <h1>选择访问方式</h1>
    <p class="subtitle">手机/异网设备若无 IPv6，请选「过 Cloudflare」；同网电脑可选「IPv6 直连」获得更低延迟。</p>
    <div class="access-list">
      <div class="access-card" id="card-ipv6">
        <div class="access-head"><div class="access-title">IPv6 直连</div><div class="access-tag">同网首选</div></div>
        <div class="access-url" id="url-ipv6">${hasIpv6 ? safe(ipv6Url) : '本机未上报公网 IPv6，仅可走 Cloudflare'}</div>
        <div class="access-actions">
          <button type="button" class="access-btn primary" id="go-ipv6">进入</button>
          <button type="button" class="access-btn" id="copy-ipv6">复制</button>
        </div>
        <div class="access-hint">不走 CF 代理，延迟最低；要求当前设备能访问该 IPv6。需 admin 凭据登录一次。</div>
      </div>
      <div class="access-card" id="card-cf">
        <div class="access-head">
          <div class="access-title">过 Cloudflare</div>
          <div class="access-tag secret">隐藏路径</div>
          <div class="access-tag cf">手机推荐</div>
        </div>
        <div class="access-url" id="url-cf">${hasIpv6 ? safe(cfUrlWithSecret) : safe(cfUrl)}</div>
        <div class="access-actions">
          <button type="button" class="access-btn primary cf" id="go-cf">进入</button>
          <button type="button" class="access-btn" id="copy-cf">复制</button>
        </div>
        <div class="access-hint">走 <code>/<span id="secret-tag">${secret ? safe(secret) : '...'}</span>/</code> 隐藏路径，仅知道完整 URL 的人能进入。需 admin 凭据登录。</div>
      </div>
    </div>
    <div class="footer"><a href="javascript:void(0)" id="back" style="color:var(--text-3);text-decoration:underline">重新输入密码</a></div>
  </div>
</div>
<div class="toast" id="toast">已复制</div>
<script>
(function(){
  var v1=document.getElementById('view-auth'),v2=document.getElementById('view-choose');
  var form=document.getElementById('form'),pw=document.getElementById('password');
  var err=document.getElementById('error'),sub=document.getElementById('submit');
  var toast=document.getElementById('toast');
  var cardIpv6=document.getElementById('card-ipv6'),cardCf=document.getElementById('card-cf');
  var urlIpv6=document.getElementById('url-ipv6'),urlCf=document.getElementById('url-cf');
  function showError(m){err.textContent=m||'验证失败';err.classList.add('show');pw.value='';pw.focus();}
  function showToast(t){toast.textContent=t||'已复制';toast.classList.add('show');clearTimeout(showToast._t);showToast._t=setTimeout(function(){toast.classList.remove('show')},1600);}
  function copy(t){if(navigator.clipboard&&navigator.clipboard.writeText)return navigator.clipboard.writeText(t);var a=document.createElement('textarea');a.value=t;a.style.position='fixed';a.style.opacity='0';document.body.appendChild(a);a.select();try{document.execCommand('copy')}catch(e){}document.body.removeChild(a);return Promise.resolve();}
  function showChoose(d){
    if(d.ipv6_url){urlIpv6.textContent=d.ipv6_url;cardIpv6.classList.remove('disabled');}else{urlIpv6.textContent='本机未上报公网 IPv6，仅可走 Cloudflare';cardIpv6.classList.add('disabled');}
    if(d.cf_url){urlCf.textContent=d.cf_url;cardCf.classList.remove('disabled');}else{urlCf.textContent='—';cardCf.classList.add('disabled');}
    v1.classList.add('hidden');v2.classList.remove('hidden');
  }
  document.getElementById('go-ipv6').onclick=function(){var u=urlIpv6.textContent.trim();if(!u||u.indexOf('http')!==0){showToast('本机无 IPv6，请走 CF');return;}window.open(u,'_blank','noopener');};
  document.getElementById('go-cf').onclick=function(){
    var u=urlCf.textContent.trim();
    if(!u||u.indexOf('http')!==0){showToast('域名不可用');return;}
    window.location.href = u;
  };
  document.getElementById('copy-ipv6').onclick=function(){var u=urlIpv6.textContent.trim();if(!u||u.indexOf('http')!==0)return;copy(u).then(function(){showToast('已复制 IPv6 链接')});};
  document.getElementById('copy-cf').onclick=function(){var u=urlCf.textContent.trim();if(!u||u.indexOf('http')!==0)return;copy(u).then(function(){showToast('已复制 Cloudflare 链接')});};
  document.getElementById('back').onclick=function(){v2.classList.add('hidden');v1.classList.remove('hidden');pw.value='';pw.focus();};
  form.addEventListener('submit',async function(e){
    e.preventDefault();err.classList.remove('show');sub.disabled=true;var t=sub.textContent;sub.textContent='验证中...';
    try{
      var r=await fetch('/verify',{method:'POST',credentials:'same-origin',headers:{'Content-Type':'application/json'},body:JSON.stringify({password:pw.value})});
      var d={};try{d=await r.json();}catch(_){}
      if(r.ok&&d.ok){
        if(!d.cf_url&&d.redirect)d.cf_url=d.redirect;
        if(!d.ipv6_url&&d.redirect)d.ipv6_url=d.redirect;
        showChoose(d);
      }else{showError(d.error||('验证失败 ('+r.status+')'));}
    }catch(_){showError('网络错误，请重试');}
    finally{sub.disabled=false;sub.textContent=t;}
  });
  pw.focus();
})();
</script>
</body>
</html>`;
}

function json(obj, status = 200, extraHeaders = {}) {
  return new Response(JSON.stringify(obj), {
    status,
    headers: {
      'Content-Type': 'application/json; charset=utf-8',
      'Cache-Control': 'no-store',
      ...extraHeaders
    }
  });
}

async function checkRate(kv, ip) {
  const key = 'attempts:' + ip;
  const raw = await kv.get(key);
  const now = Date.now();
  if (raw) {
    try {
      const rec = JSON.parse(raw);
      if (rec.lockedUntil && now < rec.lockedUntil) {
        const remain = Math.ceil((rec.lockedUntil - now) / 1000);
        return { allowed: false, message: `尝试次数过多，请 ${remain} 秒后再试` };
      }
    } catch (_) {}
  }
  return { allowed: true };
}
async function recordFailure(kv, ip) {
  const key = 'attempts:' + ip;
  const raw = await kv.get(key);
  const now = Date.now();
  let rec = { count: 0 };
  try { rec = raw ? JSON.parse(raw) : rec; } catch (_) { rec = { count: 0 }; }
  rec.count = (rec.count || 0) + 1;
  if (rec.count >= RATE_MAX) rec.lockedUntil = now + RATE_WINDOW_SEC * 1000;
  await kv.put(key, JSON.stringify(rec), { expirationTtl: RATE_WINDOW_SEC * 2 });
}
async function clearFailure(kv, ip) {
  await kv.delete('attempts:' + ip);
}

// 反代到 origin（CF 内部识别 worker fetch 跳过 worker routes）
async function proxyToIlink(request, env, url) {
  const target = env.TUNNEL_ORIGIN + url.pathname + url.search;
  const headers = new Headers(request.headers);
  headers.delete('Host');
  headers.set('X-ILink-Origin-Token', env.ORIGIN_TOKEN);
  // 透传 cf-connecting-ip
  let body;
  if (request.method !== 'GET' && request.method !== 'HEAD') {
    body = await request.arrayBuffer();
  }
  return fetch(target, {
    method: request.method,
    headers,
    body
  });
}

export default {
  async fetch(request, env, ctx) {
    const url = new URL(request.url);
    const ip = request.headers.get('cf-connecting-ip') || 'unknown';

    // 1) GET / 密码页
    if (request.method === 'GET' && url.pathname === '/') {
      const secret = await getOrCreateSecret(env.ILINK_KV);
      const latestIpv6 = await env.ILINK_KV.get(KV_KEY_IPV6);
      const chatPath = env.CHAT_PATH || CHAT_PATH_DEFAULT;
      const cfDomain = env.CF_DOMAIN || 'ilink.354199.xyz';
      const ilinkPort = env.ILINK_PORT || '80';
      const ipv6Url = env.DIRECT_URL || (latestIpv6 ? `http://[${latestIpv6}]:${ilinkPort}${chatPath}` : '');
      const hasIpv6 = !!ipv6Url;
      const cfUrl = `https://${cfDomain}${chatPath}`;
      return new Response(htmlShell({ ipv6Url, cfUrl, hasIpv6, secret }), {
        status: 200,
        headers: { 'Content-Type': 'text/html; charset=utf-8', 'Cache-Control': 'no-store' }
      });
    }

    // 2) POST /verify
    if (request.method === 'POST' && url.pathname === '/verify') {
      const rate = await checkRate(env.ILINK_KV, ip);
      if (!rate.allowed) return json({ error: rate.message }, 429);
      let password = '';
      try {
        const data = await request.json();
        password = String(data.password || '');
      } catch (_) {
        return json({ error: '请求格式错误' }, 400);
      }
      if (!env.PASSWORD) return json({ error: '服务端未配置密码' }, 500);
      if (password !== env.PASSWORD) {
        await recordFailure(env.ILINK_KV, ip);
        return json({ error: '密码错误' }, 401);
      }
      await clearFailure(env.ILINK_KV, ip);

      const secret = await getOrCreateSecret(env.ILINK_KV);
      const latestIpv6 = await env.ILINK_KV.get(KV_KEY_IPV6);
      const chatPath = env.CHAT_PATH || CHAT_PATH_DEFAULT;
      const cfDomain = env.CF_DOMAIN || 'ilink.354199.xyz';
      const ilinkPort = env.ILINK_PORT || '80';
      const ipv6Url = env.DIRECT_URL || (latestIpv6 ? `http://[${latestIpv6}]:${ilinkPort}${chatPath}` : null);
      const cfUrl = `https://${cfDomain}/${secret}${chatPath}`;

      const sessionCookie = await createGateSession(env.ILINK_KV, ip);
      return json({
        ok: true,
        ipv6_url: ipv6Url,
        cf_url: cfUrl,
        redirect: cfUrl
      }, 200, { 'Set-Cookie': sessionCookie });
    }

    // 3) GET /api/myip (a gate session is required before revealing host state)
    if (request.method === 'GET' && url.pathname === '/api/myip') {
      if (!await hasGateSession(request, env.ILINK_KV, ip)) return gateRequired(url);
      const ipv6 = await env.ILINK_KV.get(KV_KEY_IPV6);
      const lastSeen = await env.ILINK_KV.get(KV_KEY_LASTSEEN);
      return json({ ipv6: ipv6 || null, last_seen: lastSeen || null });
    }

    // 4) POST /api/report-myip
    if (request.method === 'POST' && url.pathname === '/api/report-myip') {
      const token = request.headers.get('X-Report-Token') || '';
      if (!env.REPORT_TOKEN || token !== env.REPORT_TOKEN) {
        return json({ error: 'unauthorized' }, 401);
      }
      let body = {};
      try { body = await request.json(); } catch (_) { return json({ error: 'bad json' }, 400); }
      const ipv6 = String(body.ipv6 || '').trim();
      // 空字符串 = 清空（防止路由断电/拨号掉线时密码页显示失效 URL）
      if (!ipv6) {
        await env.ILINK_KV.delete(KV_KEY_IPV6);
        await env.ILINK_KV.put(KV_KEY_LASTSEEN, new Date().toISOString());
        return json({ ok: true, cleared: true, saved_at: new Date().toISOString() });
      }
      if (!/^[0-9a-fA-F:]+$/.test(ipv6) || !ipv6.includes(':')) {
        return json({ error: 'not an ipv6 address' }, 400);
      }
      await env.ILINK_KV.put(KV_KEY_IPV6, ipv6);
      await env.ILINK_KV.put(KV_KEY_LASTSEEN, new Date().toISOString());
      return json({ ok: true, ipv6, saved_at: new Date().toISOString() });
    }

    // 4b) POST /api/rotate-secret — 重新生成 3XUI 短码
    if (request.method === 'POST' && url.pathname === '/api/rotate-secret') {
      const token = request.headers.get('X-Report-Token') || '';
      if (!env.REPORT_TOKEN || token !== env.REPORT_TOKEN) {
        return json({ error: 'unauthorized' }, 401);
      }
      const newSecret = randomSecret(7);
      await env.ILINK_KV.put(KV_KEY_SECRET, newSecret);
      return json({ ok: true, secret: newSecret, rotated_at: new Date().toISOString() });
    }

    // 5) All application routes, including API and static assets, are behind
    // the password-created session.  The secret path is defense in depth, not
    // the authentication boundary.
    if (!env.TUNNEL_ORIGIN || !env.ORIGIN_TOKEN) {
      return new Response('TUNNEL_ORIGIN or ORIGIN_TOKEN not configured', { status: 500 });
    }
    if (!await hasGateSession(request, env.ILINK_KV, ip)) return gateRequired(url);

    // 5a) secret 路径 /<short>/<path> → fetch TUNNEL_ORIGIN/<path>（反代）
    //   关键 rewrite：
    //     - 3xx Location：ilink-wm1 跳 /auth /chat 等，加回 secret 前缀（否则跳到非 secret 路径走 worker catch-all 当成 UI）
    //     - Set-Cookie Domain：把 cookie domain 改到 .354199.xyz，让 session 在 secret / 非 secret 路径都可用
    //     - HTML body：chat.html 里 <a href="/terms"> 等相对链接，加 secret 前缀（避免点链接跳出 secret）
    //   关键校验：short 必须是 KV 里的当前 secret（不是任意 6-8 位字符串都通过）
    const secretMatch = url.pathname.match(/^\/([a-z0-9]{6,8})(\/.*)?$/);
    if (secretMatch) {
      const claimedSecret = secretMatch[1];
      const rest = secretMatch[2] || '/';
      const currentSecret = await env.ILINK_KV.get(KV_KEY_SECRET);
      if (claimedSecret !== currentSecret) {
        // 不是当前有效的短码（可能是旧短码或随机猜）→ 当 UI 路径处理（返回密码页）
        // 走 5c 分支
      } else {
        const newPath = rest + url.search;
        const headers = new Headers(request.headers);
        headers.delete('Host');
        headers.set('X-ILink-Origin-Token', env.ORIGIN_TOKEN);
        let body;
        if (request.method !== 'GET' && request.method !== 'HEAD') {
          body = await request.arrayBuffer();
        }
        const response = await fetch(env.TUNNEL_ORIGIN + newPath, {
          method: request.method,
          headers,
          body
        });

        // Rewrite 3xx Location + Set-Cookie Domain
        const newHeaders = new Headers(response.headers);
        if (response.status >= 300 && response.status < 400) {
          const loc = newHeaders.get('Location');
          if (loc) {
            // 相对路径 /auth、/chat 等 → 加 secret 前缀
            if (loc.startsWith('/') && !loc.startsWith('//') && !loc.startsWith(`/${claimedSecret}/`)) {
              newHeaders.set('Location', `/${claimedSecret}${loc}`);
            }
          }
        }
        // Preserve every upstream Set-Cookie.  Combining them into one header
        // loses the remembered-device cookie on browsers that correctly parse
        // Expires commas.
        const cookies = typeof response.headers.getSetCookie === 'function'
          ? response.headers.getSetCookie()
          : (response.headers.get('Set-Cookie') ? [response.headers.get('Set-Cookie')] : []);
        newHeaders.delete('Set-Cookie');
        for (const cookie of cookies) newHeaders.append('Set-Cookie', cookie);

        // HTML body 改写：<a href="/..."> 加 secret 前缀（避免点链接跳出 secret 路径）
        const ct = newHeaders.get('Content-Type') || '';
        if (ct.includes('text/html')) {
          let html = await response.text();
          // <a href="/...">  →  <a href="/<secret>/...">（不动 /static/ /api/ 资源链接）
          html = html.replace(
            /<a\s+([^>]*?)href="(\/[^"]*)"/g,
            (m, attrs, href) => {
              if (href.startsWith('/static/') || href.startsWith('/api/') ||
                  href.startsWith(`/${claimedSecret}/`) || href === `/${claimedSecret}`) {
                return m;
              }
              return `<a ${attrs}href="/${claimedSecret}${href}"`;
            }
          );
          // 同样的 <form action="/..."> 也加 secret 前缀（如果有）
          html = html.replace(
            /<form\s+([^>]*?)action="(\/[^"]*)"/g,
            (m, attrs, href) => {
              if (href.startsWith(`/${claimedSecret}/`) || href === `/${claimedSecret}`) return m;
              return `<form ${attrs}action="/${claimedSecret}${href}"`;
            }
          );
          return new Response(html, {
            status: response.status,
            statusText: response.statusText,
            headers: newHeaders
          });
        }
        return new Response(response.body, {
          status: response.status,
          statusText: response.statusText,
          headers: newHeaders
        });
      }
    }

    // 5b) API / static resources remain usable after session verification.
    // Absolute /static URLs are intentionally proxied without the secret prefix.
    if (url.pathname.startsWith('/api/') || url.pathname.startsWith('/static/')) {
      return proxyToIlink(request, env, url);
    }

    // 5c) 其它 UI 路径（/chat /auth /static/* 等）→ 密码门（"请用根目录登录"）
    const secret = await getOrCreateSecret(env.ILINK_KV);
    const latestIpv6 = await env.ILINK_KV.get(KV_KEY_IPV6);
    const chatPath = env.CHAT_PATH || CHAT_PATH_DEFAULT;
    const cfDomain = env.CF_DOMAIN || 'ilink.354199.xyz';
    const ilinkPort = env.ILINK_PORT || '80';
    const ipv6Url = env.DIRECT_URL || (latestIpv6 ? `http://[${latestIpv6}]:${ilinkPort}${chatPath}` : '');
    const hasIpv6 = !!ipv6Url;
    const cfUrl = `https://${cfDomain}${chatPath}`;

    return new Response(htmlShell({ ipv6Url, cfUrl, hasIpv6, secret }), {
      status: 200,
      headers: { 'Content-Type': 'text/html; charset=utf-8', 'Cache-Control': 'no-store' }
    });
  }
};
