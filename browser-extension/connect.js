// Jcode Browser Agent - connect page.
// Plain-JS port of packages/extension/src/ui/connect.tsx from microsoft/playwright
// (Copyright (c) Microsoft Corporation, Apache License 2.0), modified for Jcode.
import { getExpectedToken } from './token.js';

const SUPPORTED_PROTOCOL_VERSION = 2;
const params = new URLSearchParams(location.search);
const tabGroup = params.get('tabGroup') !== 'false';
const background = params.get('background') === 'true';
const clientName = (() => {
  try {
    return JSON.parse(params.get('client') || '{}').name || 'Jcode';
  } catch {
    return 'Jcode';
  }
})();
const $ = id => document.getElementById(id);

function setStatus(kind, text, detail = '') {
  $('dot').className = 'dot' + (kind === 'connected' ? ' on' : kind === 'error' ? ' err' : '');
  $('status').textContent = text;
  $('detail').textContent = detail;
}

async function connectToTab(tab, auto = false) {
  $('approve').hidden = true;
  setStatus('connecting', 'در حال اتصال…');
  try {
    const r = await chrome.runtime.sendMessage({ type: 'connectToTab', tab, clientName, tabGroup, background, auto });
    if (r?.success) {
      setStatus('connected', `«${clientName}» وصل شد. این تب در اختیار ایجنت است.`,
          'برای توقف فوری، روی آیکن Jcode در نوار ابزار کروم بزنید و «توقف فوری» را انتخاب کنید.');
      document.title = 'Jcode ● connected';
    } else if (r?.error === 'PAUSED') {
      setStatus('error', 'ایجنت را با «توقف فوری» متوقف کرده‌اید؛ اتصال خودکار بسته است.',
          'اگر می‌خواهید Jcode دوباره به مرورگر دسترسی داشته باشد، دکمه زیر را بزنید.');
      const b = document.createElement('button');
      b.className = 'primary';
      b.style.marginTop = '10px';
      b.textContent = 'ادامه کار ایجنت';
      b.onclick = async () => {
        b.remove();
        await chrome.runtime.sendMessage({ type: 'resume' });
        connectToTab(tab, false);
      };
      $('detail').after(b);
    } else {
      setStatus('error', 'اتصال برقرار نشد.', r?.error || '');
    }
  } catch (e) {
    setStatus('error', 'اتصال برقرار نشد.', String(e));
  }
}

async function loadTabs() {
  const r = await chrome.runtime.sendMessage({ type: 'getTabs' });
  const box = $('tabs');
  box.textContent = '';
  if (!r?.success)
    return;
  for (const tab of r.tabs) {
    if (tab.id === r.currentTabId)
      continue;
    const row = document.createElement('div');
    row.className = 'tab';
    const img = document.createElement('img');
    img.src = tab.favIconUrl || 'icons/icon-16.png';
    const t = document.createElement('div');
    t.className = 't';
    t.textContent = tab.title || tab.url;
    t.title = tab.url || '';
    const b = document.createElement('button');
    b.className = 'ghost';
    b.textContent = 'انتخاب';
    b.onclick = () => connectToTab(tab);
    row.append(img, t, b);
    box.append(row);
  }
}

async function main() {
  const relayUrl = params.get('mcpRelayUrl');
  if (!relayUrl)
    return setStatus('error', 'آدرس رله (mcpRelayUrl) در لینک نیست.');
  try {
    const host = new URL(relayUrl).hostname;
    if (host !== '127.0.0.1' && host !== '[::1]')
      return setStatus('error', 'فقط اتصال محلی (127.0.0.1) مجاز است.', host);
  } catch (e) {
    return setStatus('error', 'آدرس رله نامعتبر است.', String(e));
  }
  const v = parseInt(params.get('protocolVersion') ?? '', 10);
  const requested = isNaN(v) ? 1 : v;
  if (requested !== SUPPORTED_PROTOCOL_VERSION) {
    return setStatus('error', `نسخه پروتکل پشتیبانی نمی‌شود (${requested}).`,
        `این افزونه پروتکل ${SUPPORTED_PROTOCOL_VERSION} را می‌شناسد؛ نسخه Playwright MCP را با افزونه هماهنگ کنید.`);
  }

  setStatus('connecting', `«${clientName}» می‌خواهد به مرورگر وصل شود.`);
  await chrome.runtime.sendMessage({ type: 'connectionRequested', mcpRelayUrl: relayUrl });

  const token = params.get('token');
  if (token) {
    const url = new URL(location.href);
    url.searchParams.delete('token');
    history.replaceState(null, '', url);
  }
  const expected = await getExpectedToken();
  if (token && token === expected)
    return connectToTab(undefined, true);
  if (token)
    return setStatus('error', 'توکن نادرست است.', 'توکن تنظیمات MCP با توکن افزونه (config.json) یکی نیست.');

  $('token').textContent = `PLAYWRIGHT_MCP_EXTENSION_TOKEN=${expected}`;
  $('approve').hidden = false;
  $('allowHere').onclick = () => connectToTab(undefined);
  await loadTabs();
}

// Keep the MV3 service worker (which holds the pending connection) awake.
setInterval(() => chrome.runtime.sendMessage({ type: 'keepalive' }).catch(() => {}), 20_000);
main();
