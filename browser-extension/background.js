/**
 * Jcode Browser Agent - service worker.
 *
 * Based on the Playwright Extension (packages/extension in microsoft/playwright,
 * commit 0ea36910324ed8aa2a5f460317cbd35881ee88d4), Copyright (c) Microsoft
 * Corporation, licensed under the Apache License, Version 2.0.  Ported from
 * TypeScript to plain JavaScript and modified for Jcode:
 *   - the agent's tabs live in a purple "Jcode" tab group, by default in a
 *     separate, unfocused window, so the agent does not take over the user's tabs;
 *   - every mouse / keyboard / navigation command sent over the debugger is
 *     mirrored to an in-page overlay (purple border + agent cursor + ripple);
 *   - an emergency stop (popup button) detaches the debugger from every tab.
 * See THIRD_PARTY_NOTICES.md.
 */

// ─── settings ───────────────────────────────────────────────────────────────

const GROUP_TITLE = 'Jcode';
const GROUP_TITLE_PREFIX = `${GROUP_TITLE} · `;
const GROUP_COLORS = ['purple', 'pink', 'blue', 'cyan', 'green', 'orange', 'yellow', 'red'];
const LEGACY_GROUP_TITLES = ['Playwright'];
const NON_DEBUGGABLE_SCHEMES = ['chrome:', 'edge:', 'devtools:'];
const CONNECTED_BADGE = { text: '●', color: '#A855F7', title: 'Jcode is controlling this tab' };
const CLICK_ANIMATION_MAX_MS = 450;

const DEFAULT_SETTINGS = { separateWindow: true, showOverlay: true, raiseWhenHidden: true, paused: false };

async function getSettings() {
  try {
    const stored = await chrome.storage.local.get(DEFAULT_SETTINGS);
    return { ...DEFAULT_SETTINGS, ...stored };
  } catch {
    return { ...DEFAULT_SETTINGS };
  }
}

export function debugLog(...args) {
  // eslint-disable-next-line no-console
  console.log('[Jcode]', ...args.map(a => (a && typeof a === 'object' && !(a instanceof Error)) ? JSON.stringify(a) : a));
}

function isNonDebuggableUrl(url) {
  return !!url && NON_DEBUGGABLE_SCHEMES.some(s => url.startsWith(s));
}

// ─── overlay bridge (content/overlay.js) ────────────────────────────────────

const overlayEnabled = { value: true };
getSettings().then(s => { overlayEnabled.value = s.showOverlay; });
chrome.storage.onChanged.addListener(changes => {
  if (changes.showOverlay)
    overlayEnabled.value = changes.showOverlay.newValue !== false;
});

async function ensureOverlay(tabId) {
  try {
    await chrome.scripting.executeScript({ target: { tabId, frameIds: [0] }, files: ['content/overlay.js'] });
  } catch {
    // chrome:// pages, the web store, PDF viewer... no overlay there.
  }
}

function overlayMessage(tabId, msg, timeoutMs = 0) {
  if (!overlayEnabled.value)
    return Promise.resolve();
  const send = chrome.tabs.sendMessage(tabId, { jcodeOverlay: true, ...msg }, { frameId: 0 }).catch(() => {});
  if (!timeoutMs)
    return send;
  return Promise.race([send, new Promise(r => setTimeout(r, timeoutMs))]);
}

// Mirrors a CDP command to the overlay. Returns a promise the relay awaits
// before forwarding the command (used to let the cursor arrive before a click).
function mirrorToOverlay(debuggee, method, params) {
  const tabId = debuggee?.tabId;
  if (tabId === undefined || debuggee.sessionId)
    return null;
  switch (method) {
    case 'Input.dispatchMouseEvent': {
      const { type, x, y } = params || {};
      if (type === 'mouseMoved')
        return void overlayMessage(tabId, { kind: 'move', x, y }), null;
      if (type === 'mousePressed')
        return overlayMessage(tabId, { kind: 'press', x, y, button: params.button }, CLICK_ANIMATION_MAX_MS);
      if (type === 'mouseWheel')
        return void overlayMessage(tabId, { kind: 'wheel', x, y }), null;
      return null;
    }
    case 'Input.dispatchTouchEvent': {
      const p = params?.touchPoints?.[0];
      if (p && params.type === 'touchStart')
        return overlayMessage(tabId, { kind: 'press', x: p.x, y: p.y }, CLICK_ANIMATION_MAX_MS);
      return null;
    }
    case 'Input.insertText':
    case 'Input.dispatchKeyEvent':
    case 'Input.imeSetComposition':
      return void overlayMessage(tabId, { kind: 'typing' }), null;
    case 'Page.navigate':
    case 'Page.reload':
      return void overlayMessage(tabId, { kind: 'navigate' }), null;
  }
  return null;
}


// ─── keeping the agent's page visible ───────────────────────────────────────
// Chrome stops rendering a page whose window is fully covered (or minimised),
// and Playwright then waits forever for animation frames. So the agent window
// goes to a second monitor when there is one (else the right part of the
// screen), and if it is still hidden while the agent acts, it is brought to the
// front (setting "raiseWhenHidden"; at most once every few seconds).

async function agentWindowBounds(userWindow) {
  try {
    const displays = await chrome.system.display.getInfo();
    const userCenterX = (userWindow?.left ?? 0) + (userWindow?.width ?? 0) / 2;
    const userCenterY = (userWindow?.top ?? 0) + (userWindow?.height ?? 0) / 2;
    const contains = (d, x, y) => x >= d.bounds.left && x < d.bounds.left + d.bounds.width && y >= d.bounds.top && y < d.bounds.top + d.bounds.height;
    const other = displays.find(d => !contains(d, userCenterX, userCenterY));
    if (other) {
      const w = other.workArea;
      return { left: w.left + 40, top: w.top + 40, maximize: true };
    }
    const d = displays.find(x => contains(x, userCenterX, userCenterY)) || displays[0];
    const w = d.workArea;
    const width = Math.max(900, Math.round(w.width * 0.55));
    return { left: w.left + w.width - width, top: w.top, width, height: w.height };
  } catch {
    return { width: 1100, height: 800 };
  }
}

const tabHidden = new Map();     // tabId -> true when the content script reports document.hidden
const lastRaise = new Map();     // windowId -> time of last raise

async function ensureVisible(tabId) {
  if (!tabHidden.get(tabId))
    return;
  const { raiseWhenHidden } = await getSettings();
  if (!raiseWhenHidden)
    return;
  const tab = await chrome.tabs.get(tabId).catch(() => null);
  if (!tab)
    return;
  const now = Date.now();
  if (now - (lastRaise.get(tab.windowId) || 0) < 4000)
    return;
  lastRaise.set(tab.windowId, now);
  const win = await chrome.windows.get(tab.windowId).catch(() => null);
  if (!tab.active)
    await chrome.tabs.update(tabId, { active: true }).catch(() => {});
  await chrome.windows.update(tab.windowId, { focused: true, ...(win?.state === 'minimized' ? { state: 'normal' } : {}) }).catch(() => {});
  await new Promise(r => setTimeout(r, 250));
}

// ─── relay connection (from relayConnection.ts) ─────────────────────────────

const ALLOWED_CHROME_COMMANDS = new Set([
  'chrome.debugger.attach',
  'chrome.debugger.detach',
  'chrome.debugger.sendCommand',
  'chrome.tabs.create',
  'chrome.tabs.remove',
]);

const CHROME_EVENT_METHODS = [
  'chrome.debugger.onEvent',
  'chrome.debugger.onDetach',
  'chrome.tabs.onCreated',
  'chrome.tabs.onRemoved',
];

const REATTACH_DELAY_MS = 150;
const REATTACH_VERIFY_MS = 2500;
const REATTACH_COOLDOWN_MS = 3000;

class RelayConnection {
  constructor(ws) {
    this._ws = ws;
    this._attachedTabs = new Set();
    this._hasEverAttached = false;
    this._eventListeners = [];
    this._closed = false;
    this._pendingReattach = new Set();
    this._recentReattach = new Set();
    this.onclose = undefined;
    this.ontabattached = undefined;
    this.ontabdetached = undefined;
    // Jcode: new tabs requested by the client open in this window.
    this.preferredWindowId = undefined;
    this._installEventForwarders();
    this._ws.onmessage = this._onMessage.bind(this);
    this._ws.onclose = () => this._onClose();
  }

  get attachedTabs() {
    return this._attachedTabs;
  }

  didInitialize() {
    this._sendMessage({ method: 'extension.initialized', params: [] });
  }

  close(message) {
    try {
      this._ws.close(1000, message);
    } catch {
    }
    this._onClose();
  }

  attachTab(tab) {
    if (this._closed || this._attachedTabs.has(tab.id))
      return;
    this._sendMessage({ method: 'chrome.tabs.onCreated', params: [tab] });
  }

  detachTab(tabId) {
    if (this._closed || !this._attachedTabs.has(tabId))
      return;
    detachDebugger(tabId);
    this._notifyTabDetached(tabId);
    this._sendMessage({ method: 'chrome.debugger.onDetach', params: [{ tabId }, 'target_closed'] });
    this._checkLastTabDetached();
  }

  _notifyTabAttached(tabId) {
    this._attachedTabs.add(tabId);
    this._hasEverAttached = true;
    this._pendingReattach.delete(tabId);
    this.ontabattached?.(tabId);
  }

  _notifyTabDetached(tabId) {
    this._attachedTabs.delete(tabId);
    this.ontabdetached?.(tabId);
  }

  _installEventForwarders() {
    for (const fullMethod of CHROME_EVENT_METHODS) {
      const target = resolveChromeMember(fullMethod);
      const listener = (...args) => this._onChromeEvent(fullMethod, args);
      target.obj[target.name].addListener(listener);
      this._eventListeners.push({ remove: () => target.obj[target.name].removeListener(listener) });
    }
  }

  _onClose() {
    if (this._closed)
      return;
    this._closed = true;
    this._pendingReattach.clear();
    this._recentReattach.clear();
    for (const l of this._eventListeners)
      l.remove();
    this._eventListeners = [];
    for (const tabId of [...this._attachedTabs]) {
      detachDebugger(tabId);
      this._notifyTabDetached(tabId);
    }
    this.onclose?.();
  }

  _checkLastTabDetached() {
    if (this._hasEverAttached && this._attachedTabs.size === 0 && this._pendingReattach.size === 0)
      this.close('All controlled tabs detached');
  }

  _onChromeEvent(fullMethod, args) {
    const tabId = this._tabIdForEventArgs(fullMethod, args);
    if (tabId === undefined || !this._attachedTabs.has(tabId))
      return;
    this._sendMessage({ method: fullMethod, params: args });
    if (fullMethod === 'chrome.debugger.onDetach') {
      const reason = args[1];
      this._notifyTabDetached(tabId);
      if (reason === 'target_closed' && this._maybeScheduleReattach(tabId))
        return;
      this._checkLastTabDetached();
    }
  }

  _maybeScheduleReattach(tabId) {
    if (this._closed)
      return false;
    if (this._recentReattach.has(tabId)) {
      debugLog(`Not re-attaching tab ${tabId}: re-detached within ${REATTACH_COOLDOWN_MS}ms`);
      return false;
    }
    this._recentReattach.add(tabId);
    setTimeout(() => this._recentReattach.delete(tabId), REATTACH_COOLDOWN_MS);
    this._pendingReattach.add(tabId);
    setTimeout(() => void this._tryReattach(tabId), REATTACH_DELAY_MS);
    return true;
  }

  _reattachAborted(tabId) {
    return this._closed || !this._pendingReattach.has(tabId);
  }

  async _tryReattach(tabId) {
    if (this._reattachAborted(tabId))
      return;
    let tab;
    try {
      tab = await chrome.tabs.get(tabId);
    } catch {
      this._pendingReattach.delete(tabId);
      this._checkLastTabDetached();
      return;
    }
    if (this._reattachAborted(tabId))
      return;
    if (this._attachedTabs.has(tabId)) {
      this._pendingReattach.delete(tabId);
      return;
    }
    this.attachTab(tab);
    setTimeout(() => {
      if (this._reattachAborted(tabId))
        return;
      this._pendingReattach.delete(tabId);
      if (!this._attachedTabs.has(tabId))
        this._checkLastTabDetached();
    }, REATTACH_VERIFY_MS);
  }

  _tabIdForEventArgs(fullMethod, args) {
    switch (fullMethod) {
      case 'chrome.debugger.onEvent':
      case 'chrome.debugger.onDetach':
        return args[0]?.tabId;
      case 'chrome.tabs.onCreated':
        return args[0]?.openerTabId;
      case 'chrome.tabs.onRemoved':
        return args[0];
    }
    return undefined;
  }

  _onMessage(event) {
    this._onMessageAsync(event).catch(e => debugLog('Error handling message:', e));
  }

  async _onMessageAsync(event) {
    let message;
    try {
      message = JSON.parse(event.data);
    } catch (error) {
      debugLog(`Error parsing message ${event.data}:`, error);
      this._sendError(-32700, `Error parsing message: ${error.message}`);
      return;
    }
    const response = { id: message.id };
    try {
      response.result = await this._handleCommand(message);
    } catch (error) {
      debugLog(`Error handling command ${JSON.stringify(message).slice(0, 300)}:`, error);
      response.error = error.message;
    }
    this._sendMessage(response);
  }

  async _handleCommand(message) {
    if (!ALLOWED_CHROME_COMMANDS.has(message.method))
      throw new Error(`Unknown method: ${message.method}`);
    if (this._closed)
      throw new Error('Jcode: connection was stopped');
    const args = (message.params ?? []).slice();

    // Jcode: mirror input to the overlay; let the cursor arrive before a click.
    if (message.method === 'chrome.debugger.sendCommand') {
      if (args[0]?.tabId !== undefined && this._attachedTabs.has(args[0].tabId))
        await ensureVisible(args[0].tabId);
      const wait = mirrorToOverlay(args[0], args[1], args[2]);
      if (wait)
        await wait;
      if (this._closed)
        throw new Error('Jcode: stopped by the user');
    }
    // Jcode: new tabs go to the agent's window, not the window the user is in.
    if (message.method === 'chrome.tabs.create' && this.preferredWindowId !== undefined) {
      const props = { ...(args[0] || {}) };
      if (props.windowId === undefined) {
        try {
          await chrome.windows.get(this.preferredWindowId);
          props.windowId = this.preferredWindowId;
        } catch {
          this.preferredWindowId = undefined;
        }
      }
      args[0] = props;
    }

    const result = await invokeChromeMethod(message.method, args);
    if (message.method === 'chrome.debugger.attach') {
      const target = args[0];
      if (target?.tabId !== undefined) {
        if (this._closed)
          detachDebugger(target.tabId);
        else
          this._notifyTabAttached(target.tabId);
      }
    }
    return result ?? {};
  }

  _sendError(code, message) {
    this._sendMessage({ error: { code, message } });
  }

  _sendMessage(message) {
    if (this._ws.readyState === WebSocket.OPEN)
      this._ws.send(JSON.stringify(message));
  }
}

function resolveChromeMember(fullMethod) {
  const parts = fullMethod.split('.');
  if (parts[0] !== 'chrome' || parts.length < 3)
    throw new Error(`Invalid chrome method: ${fullMethod}`);
  let obj = chrome;
  for (let i = 1; i < parts.length - 1; i++) {
    obj = obj?.[parts[i]];
    if (obj === undefined)
      throw new Error(`Unknown chrome path: ${parts.slice(0, i + 1).join('.')}, calling ${fullMethod}`);
  }
  return { obj, name: parts[parts.length - 1] };
}

function detachDebugger(tabId) {
  chrome.debugger.detach({ tabId }).catch(error => debugLog('Error detaching tab:', error?.message));
}

async function invokeChromeMethod(fullMethod, args) {
  const { obj, name } = resolveChromeMember(fullMethod);
  const fn = obj[name];
  if (typeof fn !== 'function')
    throw new Error(`Not a function: ${fullMethod}`);
  return await fn.apply(obj, args);
}

// ─── pending connections (from pendingConnection.ts) ────────────────────────

class PendingConnections {
  constructor() {
    this._map = new Map();
    chrome.tabs.onRemoved.addListener(tabId => { this._map.delete(tabId); tabHidden.delete(tabId); });
  }
  create(selectorTabId, mcpRelayUrl) {
    this._map.set(selectorTabId, mcpRelayUrl);
  }
  has(selectorTabId) {
    return this._map.has(selectorTabId);
  }
  async take(selectorTabId) {
    const url = this._map.get(selectorTabId);
    if (url === undefined)
      return undefined;
    this._map.delete(selectorTabId);
    return openRelayConnection(url);
  }
}

async function openRelayConnection(mcpRelayUrl) {
  try {
    const socket = new WebSocket(mcpRelayUrl);
    await new Promise((resolve, reject) => {
      socket.onopen = () => resolve();
      socket.onerror = () => reject(new Error('WebSocket error'));
      setTimeout(() => reject(new Error('Connection timeout')), 5000);
    });
    return new RelayConnection(socket);
  } catch (error) {
    const message = `Failed to connect to MCP relay: ${error.message}`;
    debugLog(message);
    throw new Error(message);
  }
}

// ─── tab group (from connectedTabGroup.ts) ──────────────────────────────────

function uniqueGroupStyle(clientName, taken) {
  const titles = new Set(taken.map(s => s.title));
  let title = GROUP_TITLE;
  for (let i = 2; titles.has(title); i++)
    title = `${GROUP_TITLE} (${i})`;
  const colors = new Set(taken.map(s => s.color));
  const color = GROUP_COLORS.find(c => !colors.has(c)) ?? GROUP_COLORS[0];
  return { title, color };
}

async function cleanupStaleGroups() {
  try {
    const groups = await chrome.tabGroups.query({});
    const stale = groups.filter(g => g.title === GROUP_TITLE || g.title?.startsWith(GROUP_TITLE_PREFIX) || /^Jcode \(\d+\)$/.test(g.title || '') || LEGACY_GROUP_TITLES.includes(g.title));
    const tabsPerGroup = await Promise.all(stale.map(g => chrome.tabs.query({ groupId: g.id })));
    const tabIds = tabsPerGroup.flat().map(t => t.id).filter(id => id !== undefined);
    if (tabIds.length)
      await ungroupTabs(tabIds);
  } catch (error) {
    debugLog('Error cleaning up stale groups:', error);
  }
}

class ConnectedTabGroup {
  constructor(connection, selectedTab, clientName, groupStyle, isTabReserved) {
    this.clientName = clientName;
    this.groupStyle = groupStyle;
    this._isTabReserved = isTabReserved;
    this._connection = connection;
    this._groupId = null;
    this._groupTabIds = new Set();
    this.onclose = undefined;
    this._connection.onclose = () => this._onConnectionClose();
    this._connection.ontabattached = tabId => this._onTabAttached(tabId);
    this._connection.ontabdetached = tabId => this._onTabDetached(tabId);
    this._onTabUpdatedListener = this._onTabUpdated.bind(this);
    this._onTabRemovedListener = this._onTabRemoved.bind(this);
    chrome.tabs.onUpdated.addListener(this._onTabUpdatedListener);
    chrome.tabs.onRemoved.addListener(this._onTabRemovedListener);
    this._connection.preferredWindowId = selectedTab.windowId;
    this._connection.attachTab(selectedTab);
    this._connection.didInitialize();
  }

  connectedTabIds() {
    return [...this._groupTabIds];
  }

  isControlling(tabId) {
    return this._connection.attachedTabs.has(tabId);
  }

  close(reason) {
    this._connection.close(reason);
  }

  releaseTab(tabId) {
    if (!this._groupTabIds.has(tabId))
      return;
    this._groupTabIds.delete(tabId);
    this._connection.detachTab(tabId);
  }

  _onTabUpdated(tabId, changeInfo, tab) {
    if (this.groupStyle && changeInfo.groupId !== undefined)
      this._onTabGroupChanged(tabId, tab);
    if (changeInfo.status === 'complete' && this._connection.attachedTabs.has(tabId))
      void this._showOverlay(tabId);
    if (changeInfo.url === undefined)
      return;
    if (this._connection.attachedTabs.has(tabId))
      void this._updateBadge(tabId, CONNECTED_BADGE);
    else if (this._groupTabIds.has(tabId) && !isNonDebuggableUrl(changeInfo.url))
      this._connection.attachTab(tab);
  }

  _onTabGroupChanged(tabId, tab) {
    const inOurGroup = this._groupId !== null && tab.groupId === this._groupId;
    const wasInGroup = this._groupTabIds.has(tabId);
    if (inOurGroup === wasInGroup)
      return;
    if (inOurGroup) {
      if (this._isTabReserved(tabId)) {
        void ungroupTabs([tabId]);
        return;
      }
      this._groupTabIds.add(tabId);
      if (!isNonDebuggableUrl(tab.url))
        this._connection.attachTab(tab);
    } else {
      this._groupTabIds.delete(tabId);
      if (this._connection.attachedTabs.has(tabId))
        this._connection.detachTab(tabId);
    }
  }

  _onTabRemoved(tabId) {
    this._groupTabIds.delete(tabId);
  }

  _onTabAttached(tabId) {
    void this._updateBadge(tabId, CONNECTED_BADGE);
    void this._showOverlay(tabId);
    if (this.groupStyle)
      void this._addTabToGroup(tabId);
    else
      this._groupTabIds.add(tabId);
  }

  async _showOverlay(tabId) {
    await ensureOverlay(tabId);
    await overlayMessage(tabId, { kind: 'attach', client: this.clientName || 'Jcode' });
  }

  _onTabDetached(tabId) {
    if (!this.groupStyle)
      this._groupTabIds.delete(tabId);
    void this._updateBadge(tabId, { text: '' });
    void overlayMessage(tabId, { kind: 'detach' });
  }

  _onConnectionClose() {
    chrome.tabs.onUpdated.removeListener(this._onTabUpdatedListener);
    chrome.tabs.onRemoved.removeListener(this._onTabRemovedListener);
    const groupTabs = [...this._groupTabIds];
    this._groupTabIds.clear();
    for (const tabId of groupTabs)
      void overlayMessage(tabId, { kind: 'detach' });
    if (this.groupStyle && groupTabs.length)
      void ungroupTabs(groupTabs);
    this.onclose?.();
  }

  async _updateBadge(tabId, { text, color, title }) {
    try {
      await Promise.all([
        chrome.action.setBadgeText({ tabId, text }),
        chrome.action.setTitle({ tabId, title: title || 'Jcode Browser Agent' }),
        color ? chrome.action.setBadgeBackgroundColor({ tabId, color }) : Promise.resolve(),
      ]);
    } catch {
    }
  }

  async _addTabToGroup(tabId) {
    const groupStyle = this.groupStyle;
    if (!groupStyle || this._groupTabIds.has(tabId))
      return;
    try {
      await retryOnDrag(async () => {
        if (this._groupId === null) {
          // Jcode: create the group in the tab's own window; the default is the
          // focused window, which would pull the agent's tab into the user's window.
          const { windowId } = await chrome.tabs.get(tabId);
          this._groupId = await chrome.tabs.group({ tabIds: [tabId], createProperties: { windowId } });
          await chrome.tabGroups.update(this._groupId, groupStyle);
        } else {
          await chrome.tabs.group({ groupId: this._groupId, tabIds: [tabId] });
        }
      });
      this._groupTabIds.add(tabId);
    } catch (error) {
      debugLog('Error adding tab to group:', error);
    }
  }
}

async function ungroupTabs(tabIds) {
  if (!tabIds.length)
    return;
  try {
    await retryOnDrag(() => chrome.tabs.ungroup(tabIds));
  } catch (error) {
    debugLog('Error ungrouping tabs:', error);
  }
}

async function retryOnDrag(fn) {
  const delays = [0, 100, 200, 400, 800];
  let lastError;
  for (const delay of delays) {
    if (delay)
      await new Promise(resolve => setTimeout(resolve, delay));
    try {
      await fn();
      return;
    } catch (error) {
      if (!error?.message?.includes('user may be dragging a tab'))
        throw error;
      lastError = error;
    }
  }
  throw lastError;
}

// ─── extension (from background.ts) ─────────────────────────────────────────

class JcodeExtension {
  constructor() {
    this._connections = new Map();
    this._lastConnectionId = 0;
    this._pendingConnections = new PendingConnections();
    this._lastUserWindowId = undefined;
    chrome.runtime.onMessage.addListener(this._onMessage.bind(this));
    chrome.windows.onFocusChanged.addListener(id => this._rememberUserWindow(id));
    this._cleanupPromise = cleanupStaleGroups();
  }

  async _rememberUserWindow(windowId) {
    if (windowId === chrome.windows.WINDOW_ID_NONE)
      return;
    const agentWindows = new Set([...this._connections.values()].map(g => g._connection.preferredWindowId));
    if (!agentWindows.has(windowId))
      this._lastUserWindowId = windowId;
  }

  _onMessage(message, sender, sendResponse) {
    switch (message?.type) {
      case 'connectionRequested': {
        const selectorTabId = sender.tab.id;
        this._releaseConnectPage(selectorTabId).then(() => {
          this._pendingConnections.create(selectorTabId, message.mcpRelayUrl);
          sendResponse({ success: true });
        });
        return true;
      }
      case 'getTabs':
        this._getTabs(sender.tab?.id).then(
            tabs => sendResponse({ success: true, tabs, currentTabId: sender.tab?.id }),
            error => sendResponse({ success: false, error: error.message }));
        return true;
      case 'resume':
        chrome.storage.local.set({ paused: false }).then(() => sendResponse({ success: true }));
        chrome.action.setBadgeText({ text: '' }).catch(() => {});
        return true;
      case 'connectToTab': {
        const selectedTab = message.tab ?? sender.tab;
        this._connectTab(sender.tab.id, selectedTab, message.clientName, message.tabGroup !== false, !!message.background, !!message.auto).then(
            () => sendResponse({ success: true }),
            error => sendResponse({ success: false, error: error.message }));
        return true;
      }
      case 'getConnectionStatus':
        sendResponse({
          connections: [...this._connections].map(([id, group]) => ({
            id,
            clientName: group.clientName,
            connectedTabIds: group.connectedTabIds(),
          })),
        });
        return false;
      case 'disconnect':
        this._connections.get(message.connectionId)?.close('User disconnected');
        sendResponse({ success: true });
        return false;
      case 'stopAll': {
        // Emergency stop: detach the debugger everywhere and stay paused, so the
        // agent cannot silently reconnect with its token until the user resumes.
        chrome.storage.local.set({ paused: true }).catch(() => {});
        chrome.action.setBadgeText({ text: '■' }).catch(() => {});
        chrome.action.setBadgeBackgroundColor({ color: '#DC2626' }).catch(() => {});
        const n = this._connections.size;
        for (const group of [...this._connections.values()])
          group.close('Stopped by the user (Jcode emergency stop)');
        chrome.debugger.getTargets().then(targets => {
          for (const t of targets) {
            if (t.attached && t.tabId !== undefined && t.extensionId === chrome.runtime.id)
              chrome.debugger.detach({ tabId: t.tabId }).catch(() => {});
          }
        }).catch(() => {});
        sendResponse({ success: true, stopped: n });
        return false;
      }
      case 'overlayQuery': {
        // A controlled page (re)loaded and asks whether to show the overlay.
        const tabId = sender.tab?.id;
        const group = [...this._connections.values()].find(g => g.isControlling(tabId));
        sendResponse({ active: !!group && overlayEnabled.value, client: group?.clientName || 'Jcode' });
        return false;
      }
      case 'overlayVisibility':
        if (sender.tab?.id !== undefined)
          tabHidden.set(sender.tab.id, !!message.hidden);
        return false;
      case 'keepalive':
        return false;
    }
    return false;
  }

  async _connectTab(selectorTabId, tab, clientName, tabGroup, background, auto) {
    try {
      await this._cleanupPromise;
      this._releaseTab(selectorTabId);
      if (tab.id !== selectorTabId && this._connectedTabIds().has(tab.id))
        throw new Error('This tab is already connected to another client');

      // Jcode: on the automatic (token) path the connect page itself becomes
      // the agent's tab. Move it to its own unfocused window so the user's
      // window and tabs stay untouched.
      const settings = await getSettings();
      if (settings.paused && auto)
        throw new Error('PAUSED');
      if (tab.id === selectorTabId && settings.separateWindow) {
        const userWindowId = tab.windowId;
        const srcWindow = await chrome.windows.get(userWindowId).catch(() => undefined);
        const others = srcWindow ? (await chrome.tabs.query({ windowId: userWindowId })).length : 0;
        if (others > 1) {
          const bounds = await agentWindowBounds(srcWindow);
          const agentWindow = await chrome.windows.create({ tabId: tab.id, focused: false, state: 'normal' });
          // Chrome ignores bounds passed to create() here; move it afterwards.
          await chrome.windows.update(agentWindow.id, bounds.maximize ? { left: bounds.left, top: bounds.top } : bounds).catch(() => {});
          // On another monitor: fill it (Chrome scales sizes between monitors
          // with different DPI, so an exact size is unreliable there).
          if (bounds.maximize)
            await chrome.windows.update(agentWindow.id, { state: 'maximized' }).catch(() => {});
          tab = { ...(await chrome.tabs.get(tab.id)), windowId: agentWindow.id };
          await chrome.windows.update(userWindowId, { focused: true }).catch(() => {});
        }
        background = true;
      }

      const connection = await this._pendingConnections.take(selectorTabId);
      if (!connection)
        throw new Error('Pending client connection closed');

      const id = ++this._lastConnectionId;
      const taken = [...this._connections.values()].flatMap(g => g.groupStyle ? [g.groupStyle] : []);
      const group = new ConnectedTabGroup(connection, tab, clientName, tabGroup ? uniqueGroupStyle(clientName, taken) : undefined, tabId => this._pendingConnections.has(tabId));
      group.onclose = () => this._connections.delete(id);
      this._connections.set(id, group);

      if (!background) {
        await Promise.all([
          chrome.tabs.update(tab.id, { active: true }),
          chrome.windows.update(tab.windowId, { focused: true }),
        ]).catch(() => {});
      }
      if (tab.id !== selectorTabId)
        await chrome.tabs.remove(selectorTabId).catch(() => {});
    } catch (error) {
      debugLog(`Failed to connect tab ${tab?.id}:`, error.message);
      throw error;
    }
  }

  async _releaseConnectPage(tabId) {
    this._releaseTab(tabId);
    await ungroupTabs([tabId]);
  }

  _releaseTab(tabId) {
    for (const group of this._connections.values())
      group.releaseTab(tabId);
  }

  async _getTabs(selectorTabId) {
    const tabs = await chrome.tabs.query({});
    const connected = this._connectedTabIds();
    return tabs.filter(tab => !isNonDebuggableUrl(tab.url) && (tab.id === selectorTabId || !connected.has(tab.id)));
  }

  _connectedTabIds() {
    return new Set([...this._connections.values()].flatMap(g => g.connectedTabIds()));
  }
}

new JcodeExtension();
