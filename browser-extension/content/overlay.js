/*
 * Jcode Browser Agent - in-page overlay (content script).
 *
 * Shown only on tabs the agent controls: an animated purple border, a purple
 * agent cursor that glides to where the agent clicks (with a ripple), a small
 * typing indicator and a status tag. Everything lives in a closed shadow root
 * on an aria-hidden host with pointer-events: none, so it never takes clicks
 * and stays out of the accessibility snapshot the agent reads.
 */
(() => {
  if (window.__jcodeOverlayLoaded)
    return;
  window.__jcodeOverlayLoaded = true;

  let host = null;
  let root = null;
  let els = null;
  let active = false;
  let pos = null;            // last cursor position (CSS px, viewport)
  let moveEndsAt = 0;
  let idleTimer = 0;
  let typingTimer = 0;
  const GLIDE_MS = 380;

  const CSS = `
    :host { all: initial; }
    .wrap { position: fixed; inset: 0; pointer-events: none; z-index: 2147483647; opacity: 0; transition: opacity .25s ease; }
    .wrap.on { opacity: 1; }
    .frame { position: absolute; inset: 0; border-radius: 0; padding: 3px;
      background: conic-gradient(from var(--a, 0deg), #7c3aed, #d946ef, #a855f7, #6366f1, #7c3aed);
      -webkit-mask: linear-gradient(#000 0 0) content-box, linear-gradient(#000 0 0);
      -webkit-mask-composite: xor; mask-composite: exclude;
      animation: spin 4s linear infinite; }
    .glow { position: absolute; inset: 0; box-shadow: inset 0 0 22px 6px rgba(168,85,247,.55), inset 0 0 60px 10px rgba(124,58,237,.25);
      animation: breathe 1.8s ease-in-out infinite; }
    .wrap.busy .glow { animation-duration: .9s; }
    @property --a { syntax: '<angle>'; initial-value: 0deg; inherits: false; }
    @keyframes spin { to { --a: 360deg; } }
    @keyframes breathe { 0%,100% { opacity: .55 } 50% { opacity: 1 } }
    .tag { position: absolute; top: 8px; left: 50%; transform: translateX(-50%);
      font: 600 12px/1 system-ui, "Segoe UI", Tahoma, sans-serif; color: #fff; direction: rtl;
      background: rgba(88,28,135,.92); border: 1px solid rgba(216,180,254,.9); border-radius: 999px;
      padding: 6px 12px; box-shadow: 0 4px 18px rgba(124,58,237,.45); white-space: nowrap; }
    .tag .dot { display: inline-block; width: 7px; height: 7px; border-radius: 50%; background: #e9d5ff; margin-inline-start: 6px;
      animation: breathe 1s ease-in-out infinite; vertical-align: middle; }
    .cursor { position: absolute; left: 0; top: 0; width: 0; height: 0; opacity: 0;
      transition: transform ${GLIDE_MS}ms cubic-bezier(.22,.9,.3,1), opacity .2s ease; will-change: transform; }
    .cursor.on { opacity: 1; }
    .cursor svg { position: absolute; left: -2px; top: -2px; filter: drop-shadow(0 0 6px rgba(168,85,247,.9)) drop-shadow(0 2px 3px rgba(0,0,0,.35)); }
    .cursor .name { position: absolute; left: 18px; top: 26px; font: 600 11px/1 system-ui, "Segoe UI", sans-serif; color: #fff;
      background: #7c3aed; border: 1px solid #e9d5ff; border-radius: 999px; padding: 4px 8px; white-space: nowrap; }
    .cursor .name.typing::after { content: ' · typing…'; }
    .ripple { position: absolute; width: 16px; height: 16px; margin: -8px 0 0 -8px; border-radius: 50%;
      border: 3px solid #a855f7; box-shadow: 0 0 12px rgba(217,70,239,.8); animation: ripple .6s ease-out forwards; }
    .ripple.r2 { animation-delay: .12s; opacity: 0; }
    @keyframes ripple { 0% { transform: scale(.4); opacity: 1 } 100% { transform: scale(5.5); opacity: 0 } }
    .flash { position: absolute; inset: 0; background: radial-gradient(ellipse at center, transparent 60%, rgba(168,85,247,.35));
      animation: flash .7s ease-out forwards; }
    @keyframes flash { from { opacity: 1 } to { opacity: 0 } }
    @media (prefers-reduced-motion: reduce) { .frame, .glow { animation: none; } .cursor { transition-duration: 0ms; } }
  `;

  function build() {
    if (host && host.isConnected)
      return;
    host = document.createElement('jcode-agent-overlay');
    host.setAttribute('aria-hidden', 'true');
    host.style.cssText = 'all: initial; position: fixed; inset: 0; pointer-events: none; z-index: 2147483647;';
    root = host.attachShadow({ mode: 'closed' });
    root.innerHTML = `
      <style>${CSS}</style>
      <div class="wrap">
        <div class="glow"></div>
        <div class="frame"></div>
        <div class="tag">Jcode روی این تب کار می‌کند<span class="dot"></span></div>
        <div class="cursor">
          <svg width="26" height="34" viewBox="0 0 13 17"><path d="M1 1 L1 14.5 L4.3 11.4 L6.6 16.3 L8.6 15.4 L6.4 10.6 L10.8 10.6 Z"
            fill="#9333ea" stroke="#fff" stroke-width="1.1" stroke-linejoin="round"/></svg>
          <div class="name">Jcode</div>
        </div>
      </div>`;
    els = {
      wrap: root.querySelector('.wrap'),
      cursor: root.querySelector('.cursor'),
      name: root.querySelector('.name'),
      tag: root.querySelector('.tag'),
    };
    (document.documentElement || document).appendChild(host);
  }

  // Pages sometimes wipe <html> children (SPA frameworks, document.write);
  // re-append the host while active.
  const keepAlive = setInterval(() => {
    if (active && host && !host.isConnected && document.documentElement)
      document.documentElement.appendChild(host);
  }, 1000);

  function reportVisibility() {
    try {
      chrome.runtime.sendMessage({ type: 'overlayVisibility', hidden: document.visibilityState === 'hidden' }).catch(() => {});
    } catch {
    }
  }
  document.addEventListener('visibilitychange', () => { if (active) reportVisibility(); });

  function show(client) {
    build();
    if (!active)
      reportVisibility();
    active = true;
    if (client)
      els.tag.firstChild.textContent = `${client === 'Jcode' || !client ? 'Jcode' : client} روی این تب کار می‌کند`;
    requestAnimationFrame(() => els.wrap.classList.add('on'));
  }

  function hide() {
    active = false;
    if (!els)
      return;
    els.wrap.classList.remove('on', 'busy');
    els.cursor.classList.remove('on');
  }

  function busy() {
    if (!els)
      return;
    els.wrap.classList.add('busy');
    clearTimeout(idleTimer);
    idleTimer = setTimeout(() => els && els.wrap.classList.remove('busy'), 3000);
  }

  function moveTo(x, y) {
    if (!active || typeof x !== 'number' || typeof y !== 'number')
      return 0;
    const c = els.cursor;
    if (!pos) {
      // first appearance: start a little up-left of the target, then glide in
      c.style.transition = 'none';
      c.style.transform = `translate(${x - 120}px, ${y - 80}px)`;
      void c.offsetWidth;
      c.style.transition = '';
    }
    const dist = pos ? Math.hypot(x - pos.x, y - pos.y) : 150;
    pos = { x, y };
    c.classList.add('on');
    c.style.transform = `translate(${x}px, ${y}px)`;
    if (dist < 2)
      return 0;
    moveEndsAt = performance.now() + GLIDE_MS;
    return GLIDE_MS;
  }

  function ripple(x, y) {
    const wrap = els.wrap;
    for (const cls of ['ripple', 'ripple r2']) {
      const r = document.createElement('div');
      r.className = cls;
      r.style.left = `${x}px`;
      r.style.top = `${y}px`;
      wrap.appendChild(r);
      setTimeout(() => r.remove(), 900);
    }
  }

  function typing() {
    if (!els)
      return;
    els.name.classList.add('typing');
    clearTimeout(typingTimer);
    typingTimer = setTimeout(() => els && els.name.classList.remove('typing'), 900);
  }

  function flash() {
    const f = document.createElement('div');
    f.className = 'flash';
    els.wrap.appendChild(f);
    setTimeout(() => f.remove(), 800);
  }

  chrome.runtime.onMessage.addListener((msg, _sender, sendResponse) => {
    if (!msg || !msg.jcodeOverlay)
      return false;
    switch (msg.kind) {
      case 'attach':
        show(msg.client);
        sendResponse({ ok: true });
        return false;
      case 'detach':
        hide();
        sendResponse({ ok: true });
        return false;
    }
    if (!active)
      show();
    busy();
    switch (msg.kind) {
      case 'move':
      case 'wheel':
        moveTo(msg.x, msg.y);
        sendResponse({ ok: true });
        return false;
      case 'press': {
        // Make sure the cursor is (or gets) there, then ripple and let the
        // click go through. The service worker waits at most ~450 ms.
        let wait = 0;
        if (!pos || Math.hypot(msg.x - pos.x, msg.y - pos.y) >= 2)
          wait = moveTo(msg.x, msg.y);
        else
          wait = Math.max(0, moveEndsAt - performance.now());
        setTimeout(() => {
          ripple(msg.x, msg.y);
          sendResponse({ ok: true });
        }, Math.min(wait, 400));
        return true;
      }
      case 'typing':
        typing();
        sendResponse({ ok: true });
        return false;
      case 'navigate':
        flash();
        sendResponse({ ok: true });
        return false;
    }
    return false;
  });

  // After a navigation the page is new: ask whether this tab is controlled.
  function query() {
    try {
      chrome.runtime.sendMessage({ type: 'overlayQuery' }).then(r => {
        if (r && r.active)
          show(r.client);
      }).catch(() => {});
    } catch {
      clearInterval(keepAlive);  // extension was reloaded; this script is orphaned
    }
  }
  if (document.readyState === 'loading')
    document.addEventListener('DOMContentLoaded', query, { once: true });
  else
    query();
})();
