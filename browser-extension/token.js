// The token lets Jcode connect without the approval page. It comes from
// config.json next to the manifest (written by the setup script and shared with
// the MCP launcher); if that file is missing, a random token is kept in storage.
function randomToken() {
  const a = new Uint8Array(32);
  crypto.getRandomValues(a);
  return btoa(String.fromCharCode(...a)).replace(/[+/=]/g, m => ({ '+': '-', '/': '_', '=': '' })[m]);
}

export async function getExpectedToken() {
  try {
    const r = await fetch(chrome.runtime.getURL('config.json'), { cache: 'no-store' });
    if (r.ok) {
      const cfg = await r.json();
      if (typeof cfg.token === 'string' && cfg.token.length >= 16)
        return cfg.token;
    }
  } catch {
  }
  const { authToken } = await chrome.storage.local.get('authToken');
  if (authToken)
    return authToken;
  const t = randomToken();
  await chrome.storage.local.set({ authToken: t });
  return t;
}
