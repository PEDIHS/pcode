# Third-party notices — Jcode Browser Agent

## Playwright Extension (Microsoft)

- Source: https://github.com/microsoft/playwright/tree/main/packages/extension
  (commit `0ea36910324ed8aa2a5f460317cbd35881ee88d4`, extension version 0.5.0).
  This is the successor of the "Playwright MCP Bridge" extension that used to
  live in https://github.com/microsoft/playwright-mcp (`extension/` folder).
- Copyright (c) Microsoft Corporation.
- License: Apache License, Version 2.0 — full text in `LICENSE-Apache-2.0.txt`.

Files derived from it:

| Jcode file | Upstream file(s) | Changes |
|---|---|---|
| `background.js` | `src/background.ts`, `src/relayConnection.ts`, `src/pendingConnection.ts`, `src/connectedTabGroup.ts` | Ported from TypeScript to plain JavaScript (no build step). Tab group renamed to "Jcode" with purple as the first colour; on the automatic (token) path the agent's tab is moved to its own unfocused window and new tabs open there; mouse/keyboard/navigation commands sent through `chrome.debugger` are mirrored to an in-page overlay (a click waits up to 450 ms so the cursor is seen arriving); added an emergency `stopAll` message, an `overlayQuery` message, and the `scripting`/`storage` permissions. |
| `connect.html`, `connect.js`, `token.js` | `src/ui/connect.html`, `src/ui/connect.tsx`, `src/ui/authToken.tsx` | Rewritten without React, Persian (RTL) UI; the expected token is read from `config.json` (shared with the Jcode MCP launcher), falling back to a random token in `chrome.storage`. |
| `manifest.json` | `manifest.json` | New name, icons, popup, content script and permissions; a different `key`, so the extension id is `jmnejhlgjkioenaidflmjohpgcohbbli` and does not clash with the Web Store Playwright Extension. |

Written for Jcode (not derived): `content/overlay.js`, `popup.html`, `popup.js`,
`ui.css`, `icons/*`.

The MCP server side is Playwright MCP (`@playwright/mcp`, Apache-2.0, Microsoft),
used unmodified except for the extension id — see
`D:\jcode-build\jcode-browser-mcp\THIRD_PARTY_NOTICES.md`.

## Alternatives that were considered

- **BrowserMCP** (https://github.com/BrowserMCP/mcp, Apache-2.0): also open
  source (server, daemon and extension, with a self-build option), and also
  drives tabs with Playwright through a `chrome.debugger` relay. Not chosen
  because it adds a per-user daemon and a pairing step, and Jcode already runs
  Playwright MCP (`@playwright/mcp@0.0.84`), whose extension protocol this
  extension speaks directly.
- **Playwright Extension / Playwright MCP Bridge** was chosen: Apache-2.0, full
  source available, maintained by Microsoft, already has tab groups, a
  token-based auto-connect and a well-tested relay to Playwright MCP, which
  gives Jcode the complete Playwright tool set (snapshots with element refs,
  click, type, fill forms, tabs, screenshots, …).
