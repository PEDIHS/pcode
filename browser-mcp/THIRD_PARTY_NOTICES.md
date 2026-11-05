# Third-party notices — Jcode Browser MCP launcher

## Playwright MCP / Playwright (Microsoft)

- Packages: `@playwright/mcp` 0.0.84 and its dependency `playwright-core`
  1.65.0-alpha-2026-10-30, installed from npm into `node_modules/`.
- Source: https://github.com/microsoft/playwright-mcp and https://github.com/microsoft/playwright
- Copyright (c) Microsoft Corporation.
- License: Apache License, Version 2.0 — full text in `LICENSE-Apache-2.0.txt`.

Modification: `launch.cjs` rewrites one constant, `playwrightExtensionId`, in the
installed `playwright-core/lib/coreBundle.js` and
`playwright-core/lib/tools/utils/extension.js`, from the Chrome Web Store id of
the Playwright Extension to the id of the Jcode Browser Agent extension
(`jmnejhlgjkioenaidflmjohpgcohbbli`). Nothing else in Playwright is changed.
