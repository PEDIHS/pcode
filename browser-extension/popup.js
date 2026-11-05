const $ = id => document.getElementById(id);
$('ver').textContent = chrome.runtime.getManifest().version;

async function refresh() {
  const r = await chrome.runtime.sendMessage({ type: 'getConnectionStatus' }).catch(() => null);
  const conns = r?.connections || [];
  const tabIds = conns.flatMap(c => c.connectedTabIds);
  $('dot').className = 'dot' + (conns.length ? ' on' : '');
  $('status').textContent = conns.length ? `وصل است — ایجنت روی ${tabIds.length} تب کار می‌کند` : 'وصل نیست';
  $('detail').textContent = conns.length
    ? conns.map(c => c.clientName || 'Jcode').join('، ')
    : 'وقتی Jcode از ابزار مرورگر استفاده کند، خودکار وصل می‌شود.';
  const { paused } = await chrome.storage.local.get({ paused: false });
  $('pausedBox').hidden = !paused;
  $('stop').disabled = !conns.length && paused;
  const box = $('tabs');
  box.textContent = '';
  for (const id of tabIds) {
    const tab = await chrome.tabs.get(id).catch(() => null);
    if (!tab)
      continue;
    const row = document.createElement('div');
    row.className = 'tab';
    const img = document.createElement('img');
    img.src = tab.favIconUrl || 'icons/icon-16.png';
    const t = document.createElement('div');
    t.className = 't';
    t.textContent = tab.title || tab.url;
    const b = document.createElement('button');
    b.className = 'ghost';
    b.textContent = 'نمایش';
    b.onclick = async () => {
      await chrome.windows.update(tab.windowId, { focused: true });
      await chrome.tabs.update(tab.id, { active: true });
    };
    row.append(img, t, b);
    box.append(row);
  }
}

$('resume').onclick = async () => {
  await chrome.runtime.sendMessage({ type: 'resume' });
  refresh();
};

$('stop').onclick = async () => {
  $('stop').disabled = true;
  const r = await chrome.runtime.sendMessage({ type: 'stopAll' }).catch(() => null);
  $('status').textContent = r?.success ? 'متوقف شد. ایجنت دیگر به مرورگر دسترسی ندارد.' : 'خطا در توقف';
  setTimeout(refresh, 600);
};

const settings = await chrome.storage.local.get({ separateWindow: true, showOverlay: true, raiseWhenHidden: true });
for (const key of ['separateWindow', 'showOverlay', 'raiseWhenHidden']) {
  $(key).checked = settings[key];
  $(key).onchange = () => chrome.storage.local.set({ [key]: $(key).checked });
}
refresh();
setInterval(refresh, 1500);
