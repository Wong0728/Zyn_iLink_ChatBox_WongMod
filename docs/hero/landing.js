/* ── Landing Page Scripts ───────────────────────────────── */

(function () {
  'use strict';

  var SUN_SVG = '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round"><circle cx="12" cy="12" r="4"/><path d="M12 2v2M12 20v2M4.93 4.93l1.41 1.41M17.66 17.66l1.41 1.41M2 12h2M20 12h2M6.34 17.66l-1.41 1.41M19.07 4.93l-1.41 1.41"/></svg>';
  var MOON_SVG = '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round"><path d="M21 12.79A9 9 0 1111.21 3 7 7 0 0021 12.79z"/></svg>';

  // ── Scroll-reveal via IntersectionObserver ────────────
  function initReveal() {
    var reveals = document.querySelectorAll('.reveal');
    if (!reveals.length) return;

    if (window.matchMedia('(prefers-reduced-motion: reduce)').matches) {
      reveals.forEach(function (el) { el.classList.add('visible'); });
      return;
    }

    var observer = new IntersectionObserver(function (entries) {
      entries.forEach(function (entry) {
        if (entry.isIntersecting) {
          entry.target.classList.add('visible');
          observer.unobserve(entry.target);
        }
      });
    }, {
      threshold: 0.12,
      rootMargin: '0px 0px -40px 0px'
    });

    reveals.forEach(function (el) { observer.observe(el); });
  }

  // ── Mobile nav toggle ─────────────────────────────────
  function initMobileNav() {
    var toggle = document.querySelector('.nav-mobile-toggle');
    var menu = document.getElementById('mobile-menu');
    var backdrop = document.querySelector('.mobile-menu-backdrop');
    if (!toggle || !menu) return;

    function setOpen(open) {
      toggle.setAttribute('aria-expanded', open);
      menu.classList.toggle('open', open);
      menu.setAttribute('aria-hidden', !open);
    }

    toggle.addEventListener('click', function () {
      setOpen(!menu.classList.contains('open'));
    });

    if (backdrop) {
      backdrop.addEventListener('click', function () {
        setOpen(false);
      });
    }

    menu.querySelectorAll('a').forEach(function (link) {
      link.addEventListener('click', function () {
        setOpen(false);
      });
    });
  }

  // ── Copy button for code block ────────────────────────
  function initCopyButton() {
    var btn = document.querySelector('.hero-code-copy');
    if (!btn) return;

    btn.addEventListener('click', function () {
      var text = './ilink-wm1';
      if (navigator.clipboard && navigator.clipboard.writeText) {
        navigator.clipboard.writeText(text).then(function () {
          showCopied(btn);
        }).catch(function () {
          fallbackCopy(text, btn);
        });
      } else {
        fallbackCopy(text, btn);
      }
    });
  }

  function showCopied(btn) {
    var original = btn.textContent;
    btn.textContent = '已复制';
    btn.classList.add('copied');
    setTimeout(function () {
      btn.textContent = original;
      btn.classList.remove('copied');
    }, 2000);
  }

  function fallbackCopy(text, btn) {
    var ta = document.createElement('textarea');
    ta.value = text;
    ta.style.position = 'fixed';
    ta.style.opacity = '0';
    document.body.appendChild(ta);
    ta.select();
    try { document.execCommand('copy'); showCopied(btn); }
    catch (_) { /* ignore */ }
    document.body.removeChild(ta);
  }

  // ── Nav background on scroll ──────────────────────────
  function initNavScroll() {
    var nav = document.querySelector('.nav');
    if (!nav) return;
    var scrolled = false;
    window.addEventListener('scroll', function () {
      var y = window.scrollY || window.pageYOffset;
      if (y > 20 && !scrolled) {
        nav.classList.add('nav--scrolled');
        scrolled = true;
      } else if (y <= 20 && scrolled) {
        nav.classList.remove('nav--scrolled');
        scrolled = false;
      }
    }, { passive: true });
  }

  // ── Smooth scroll for anchor links ────────────────────
  function initSmoothScroll() {
    document.querySelectorAll('a[href^="#"]').forEach(function (link) {
      link.addEventListener('click', function (e) {
        var id = link.getAttribute('href').slice(1);
        var target = document.getElementById(id);
        if (target) {
          e.preventDefault();
          var offset = parseInt(getComputedStyle(document.documentElement).getPropertyValue('--nav-height'), 10) || 64;
          var top = target.getBoundingClientRect().top + (window.scrollY || window.pageYOffset) - offset - 20;
          window.scrollTo({ top: top, behavior: 'smooth' });
        }
      });
    });
  }

  // ── Theme toggle ──────────────────────────────────────
  function initThemeToggle() {
    var root = document.documentElement;
    var btn = document.querySelector('.nav-theme-toggle');
    if (!btn) return;

    function syncIcon() {
      var t = root.getAttribute('data-theme') || 'dark';
      btn.innerHTML = t === 'dark' ? SUN_SVG : MOON_SVG;
    }
    syncIcon();

    btn.addEventListener('click', function () {
      var cur = root.getAttribute('data-theme') || 'dark';
      var next = cur === 'dark' ? 'light' : 'dark';
      root.setAttribute('data-theme', next);
      try { localStorage.setItem('ilink-theme', next); } catch (e) { /* ignore */ }
      var m = document.querySelector('meta[name="theme-color"]');
      if (m) m.setAttribute('content', next === 'dark' ? '#0a0a0a' : '#ffffff');
      syncIcon();
      root.dispatchEvent(new CustomEvent('themechange', { detail: { theme: next } }));
    });

    // Follow system changes only when the user hasn't set a preference.
    try {
      var mq = window.matchMedia('(prefers-color-scheme: dark)');
      mq.addEventListener('change', function (e) {
        if (localStorage.getItem('ilink-theme')) return;
        var t = e.matches ? 'dark' : 'light';
        root.setAttribute('data-theme', t);
        var m = document.querySelector('meta[name="theme-color"]');
        if (m) m.setAttribute('content', t === 'dark' ? '#0a0a0a' : '#ffffff');
        syncIcon();
        root.dispatchEvent(new CustomEvent('themechange', { detail: { theme: t } }));
      });
    } catch (e) { /* ignore */ }
  }

  // ── Interactive dot-grid hero background ──────────────
  // A field of faint dots; dots near the pointer light up in the accent
  // color with a smooth radial falloff (strongest at the cursor, fading out).
  function initDotGrid() {
    var canvas = document.getElementById('hero-dots');
    if (!canvas) return;
    var heroBg = canvas.parentElement;
    var ctx = canvas.getContext('2d');
    if (!ctx) return;

    var reduceMotion = window.matchMedia('(prefers-reduced-motion: reduce)').matches;

    var W = 0, H = 0;
    var dpr = Math.min(window.devicePixelRatio || 1, 2);
    var dots = [];
    var spacing = 26;
    var dotR = 1.5;
    var radius = 200;

    var target = { x: -99999, y: -99999, active: false };
    var cur = { x: -99999, y: -99999 };
    var intensity = 0;
    var raf = 0;
    var running = false;
    var lastMove = 0;

    function readColors() {
      var cs = getComputedStyle(document.documentElement);
      var t = document.documentElement.getAttribute('data-theme');
      return {
        base: (cs.getPropertyValue('--text-tertiary').trim()) || '#6e6e73',
        accent: (cs.getPropertyValue('--accent').trim()) || '#34C759',
        baseAlpha: parseFloat(cs.getPropertyValue('--dot-base-alpha')) || (t === 'light' ? 0.10 : 0.16)
      };
    }
    var col = readColors();

    function setup() {
      var rect = heroBg.getBoundingClientRect();
      W = rect.width;
      H = rect.height;
      dpr = Math.min(window.devicePixelRatio || 1, 2);
      canvas.width = Math.max(1, Math.floor(W * dpr));
      canvas.height = Math.max(1, Math.floor(H * dpr));
      canvas.style.width = W + 'px';
      canvas.style.height = H + 'px';
      ctx.setTransform(dpr, 0, 0, dpr, 0, 0);

      var cols = Math.max(1, Math.ceil(W / spacing) + 1);
      var rows = Math.max(1, Math.ceil(H / spacing) + 1);
      var ox = (W - (cols - 1) * spacing) / 2;
      var oy = (H - (rows - 1) * spacing) / 2;
      dots = [];
      for (var r = 0; r < rows; r++) {
        for (var c = 0; c < cols; c++) {
          dots.push({ x: ox + c * spacing, y: oy + r * spacing });
        }
      }
      draw();
    }

    function draw() {
      ctx.clearRect(0, 0, W, H);
      var rad2 = radius * radius;
      var i, d, dx, dy, dist2, t, e, amt;

      // Pass 1 — base grid.
      ctx.fillStyle = col.base;
      ctx.globalAlpha = col.baseAlpha;
      for (i = 0; i < dots.length; i++) {
        d = dots[i];
        ctx.beginPath();
        ctx.arc(d.x, d.y, dotR, 0, Math.PI * 2);
        ctx.fill();
      }

      // Pass 2 — accent highlight near the pointer.
      if (intensity > 0.005) {
        ctx.fillStyle = col.accent;
        for (i = 0; i < dots.length; i++) {
          d = dots[i];
          dx = d.x - cur.x;
          dy = d.y - cur.y;
          dist2 = dx * dx + dy * dy;
          if (dist2 >= rad2) continue;
          t = 1 - Math.sqrt(dist2) / radius;       // 1 at center -> 0 at edge
          e = t * t * (3 - 2 * t);                  // smoothstep
          amt = e * intensity;
          if (amt <= 0.01) continue;
          ctx.globalAlpha = amt * 0.85;
          ctx.beginPath();
          ctx.arc(d.x, d.y, dotR + amt * 2.2, 0, Math.PI * 2);
          ctx.fill();
        }
      }
      ctx.globalAlpha = 1;
    }

    function tick() {
      var dx = target.x - cur.x;
      var dy = target.y - cur.y;
      cur.x += dx * 0.2;
      cur.y += dy * 0.2;
      var des = target.active ? 1 : 0;
      intensity += (des - intensity) * 0.12;
      draw();

      var settled = Math.abs(dx) < 0.4 && Math.abs(dy) < 0.4 &&
        Math.abs(des - intensity) < 0.01 && performance.now() - lastMove > 90;
      if (settled) {
        running = false;
        raf = 0;
        return;
      }
      raf = requestAnimationFrame(tick);
    }

    function ensureRunning() {
      lastMove = performance.now();
      if (!running) {
        running = true;
        raf = requestAnimationFrame(tick);
      }
    }

    function onMove(e) {
      var r = canvas.getBoundingClientRect();
      target.x = e.clientX - r.left;
      target.y = e.clientY - r.top;
      target.active = true;
      ensureRunning();
    }

    function onOut(e) {
      // relatedTarget === null means the pointer left the viewport entirely.
      if (e.relatedTarget === null) {
        target.active = false;
        ensureRunning();
      }
    }

    setup();
    if (!reduceMotion) {
      window.addEventListener('pointermove', onMove, { passive: true });
      window.addEventListener('pointerout', onOut, { passive: true });
    }
    var ro = new ResizeObserver(function () { setup(); });
    ro.observe(heroBg);

    // Re-read colors when the theme changes and redraw.
    document.documentElement.addEventListener('themechange', function () {
      col = readColors();
      draw();
    });
  }

  // ── Init all ──────────────────────────────────────────
  function init() {
    initThemeToggle();
    initReveal();
    initMobileNav();
    initCopyButton();
    initNavScroll();
    initSmoothScroll();
    initDotGrid();
  }

  if (document.readyState === 'loading') {
    document.addEventListener('DOMContentLoaded', init);
  } else {
    init();
  }
})();
