(function () {
  'use strict';

  /*
   * Lightweight client-side router.
   *
   * Intercepts clicks on same-origin links, fetches the target page,
   * and swaps only `<main class="layout">` plus the navigation active
   * states. The header, footer, and the WebGL canvas are never torn
   * down, so the background animation runs continuously across page
   * transitions.
   *
   * Falls back to a full browser navigation on any error.
   */

  if (!window.history || !window.fetch) return;

  var MAIN_SEL = 'main.layout';
  var NAV_SEL = '.top-nav';

  var cache = Object.create(null);
  var cacheOrder = [];
  var MAX_CACHE_PAGES = 12;
  var inflight = null;

  function normalizePath(path) {
    var u = new URL(path, window.location.origin);
    return u.pathname + u.search;
  }

  function remember(key, html) {
    if (!cache[key]) cacheOrder.push(key);
    cache[key] = html;
    while (cacheOrder.length > MAX_CACHE_PAGES) {
      var old = cacheOrder.shift();
      if (old !== key) delete cache[old];
    }
  }

  function swap(html, url) {
    var doc;
    try {
      doc = new DOMParser().parseFromString(html, 'text/html');
    } catch (e) {
      window.location.href = url;
      return;
    }

    var newMain = doc.querySelector(MAIN_SEL);
    var curMain = document.querySelector(MAIN_SEL);
    if (!newMain || !curMain) {
      window.location.href = url;
      return;
    }

    /* Title */
    var t = doc.querySelector('title');
    if (t) document.title = t.textContent;

    /* Top nav (active class only changes between pages) */
    var newNav = doc.querySelector(NAV_SEL);
    var curNav = document.querySelector(NAV_SEL);
    if (newNav && curNav) curNav.innerHTML = newNav.innerHTML;

    /* Swap entire main so layout class (has-sidebar / no-sidebar) and sidebar update */
    curMain.replaceWith(newMain);

    /* Scroll handling: hash anchor or top */
    var hash = (new URL(url, window.location.href)).hash;
    if (hash) {
      var target = document.querySelector(hash);
      if (target) target.scrollIntoView();
      else window.scrollTo(0, 0);
    } else {
      window.scrollTo(0, 0);
    }
  }

  function loadPath(url, push) {
    var key = normalizePath(url);

    /* Cache hit - synchronous swap, no flicker */
    if (cache[key]) {
      if (push) window.history.pushState({ url: url }, '', url);
      swap(cache[key], url);
      document.dispatchEvent(new CustomEvent('aion:navigated', { detail: { url: url } }));
      return;
    }

    if (inflight) inflight.abort();
    var ctrl = (typeof AbortController !== 'undefined') ? new AbortController() : null;
    inflight = ctrl;
    var opts = { credentials: 'same-origin' };
    if (ctrl) opts.signal = ctrl.signal;

    fetch(url, opts)
      .then(function (r) {
        if (!r.ok) throw new Error('http ' + r.status);
        return r.text();
      })
      .then(function (html) {
        remember(key, html);
        if (push) window.history.pushState({ url: url }, '', url);
        swap(html, url);
        document.dispatchEvent(new CustomEvent('aion:navigated', { detail: { url: url } }));
      })
      .catch(function (err) {
        if (err && err.name === 'AbortError') return;
        window.location.href = url;
      });
  }

  /*
   * Keep the current document available for back/forward without warming the
   * whole documentation tree. The old eager prefetch kept every generated page
   * in memory, which made long browsing sessions heavier than needed.
   */
  function seedCurrentPage() {
    try {
      var here = window.location.pathname + window.location.search;
      remember(here, '<!doctype html>' + document.documentElement.outerHTML);
    } catch (_) { /* fine */ }
  }

  function handleClick(e) {
    if (e.defaultPrevented) return;
    if (e.button !== 0) return;
    if (e.metaKey || e.ctrlKey || e.altKey || e.shiftKey) return;

    var el = e.target;
    while (el && el.tagName !== 'A') el = el.parentNode;
    if (!el || el.tagName !== 'A') return;
    if (el.hasAttribute('download')) return;
    var target = el.getAttribute('target');
    if (target && target !== '_self') return;

    var href = el.getAttribute('href');
    if (!href) return;

    var url;
    try {
      url = new URL(el.href, window.location.href);
    } catch (_) {
      return;
    }
    if (url.origin !== window.location.origin) return;

    if (url.pathname === window.location.pathname && url.search === window.location.search) {
      if (url.hash) return;
      e.preventDefault();
      return;
    }

    e.preventDefault();
    loadPath(url.pathname + url.search + url.hash, true);
  }

  document.addEventListener('click', handleClick);

  window.addEventListener('popstate', function () {
    var u = window.location.pathname + window.location.search + window.location.hash;
    loadPath(u, false);
  });

  /* Seed after first paint without preloading the entire site. */
  if (document.readyState === 'complete') seedCurrentPage();
  else window.addEventListener('load', seedCurrentPage, { once: true });
})();
