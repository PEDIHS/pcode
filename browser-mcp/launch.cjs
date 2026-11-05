#!/usr/bin/env node
/*
 * Jcode Browser MCP launcher.
 *
 * Runs Playwright MCP (Apache-2.0, pinned to @playwright/mcp 0.0.84 in
 * package.json) in --extension mode, connected to the "Jcode Browser Agent"
 * extension instead of the Chrome Web Store "Playwright Extension":
 *
 *   1. The relay opens chrome-extension://<id>/connect.html, with the Web Store
 *      extension id hard-coded.  We rewrite that id in the installed copy of
 *      playwright-core to our extension's id (idempotent; re-applied after every
 *      npm install, refuses to run if the id is not found).
 *   2. The shared token from the extension's config.json is passed as
 *      PLAYWRIGHT_MCP_EXTENSION_TOKEN, so connecting needs no approval click.
 *
 * Environment (optional):
 *   JCODE_EXTENSION_DIR   extension folder (default ../jcode-browser-extension)
 *   JCODE_BROWSER_EXE     browser executable (default: installed Google Chrome)
 *   JCODE_BROWSER_PROFILE user-data-dir for that browser (default: its normal profile)
 */
'use strict';

const fs = require('fs');
const path = require('path');
const crypto = require('crypto');

const PLAYWRIGHT_STORE_ID = 'mmlmfjhmonkocbjadbfplnigmagldckm';
const extDir = process.env.JCODE_EXTENSION_DIR || path.resolve(__dirname, '..', 'jcode-browser-extension');

function fail(msg) {
  process.stderr.write(`[jcode-browser-mcp] ${msg}\n`);
  process.exit(1);
}

function extensionIdFromManifest() {
  const manifest = JSON.parse(fs.readFileSync(path.join(extDir, 'manifest.json'), 'utf8'));
  if (!manifest.key)
    fail('manifest.json has no "key"; cannot compute the extension id');
  const der = Buffer.from(manifest.key, 'base64');
  const hex = crypto.createHash('sha256').update(der).digest('hex').slice(0, 32);
  return [...hex].map(c => String.fromCharCode(97 + parseInt(c, 16))).join('');
}

function patchExtensionId(id) {
  const mcpDir = path.dirname(require.resolve('@playwright/mcp/package.json'));
  const libDir = path.dirname(require.resolve('playwright-core/lib/coreBundle', { paths: [mcpDir] }));
  const files = [path.join(libDir, 'coreBundle.js'), path.join(libDir, 'tools', 'utils', 'extension.js')];
  const original = `playwrightExtensionId = "${PLAYWRIGHT_STORE_ID}"`;
  const marker = `playwrightExtensionId = "${id}"`;
  for (const file of files) {
    if (!fs.existsSync(file))
      continue;
    const src = fs.readFileSync(file, 'utf8');
    if (src.includes(marker))
      continue;
    if (!src.includes(original))
      fail(`could not find the extension id in ${file}; the pinned @playwright/mcp version changed`);
    fs.writeFileSync(file + '.tmp', src.split(original).join(marker));
    fs.renameSync(file + '.tmp', file);
    process.stderr.write(`[jcode-browser-mcp] patched extension id in ${path.basename(file)} -> ${id}
`);
  }
}

function readToken() {
  try {
    const cfg = JSON.parse(fs.readFileSync(path.join(extDir, 'config.json'), 'utf8'));
    return typeof cfg.token === 'string' ? cfg.token : undefined;
  } catch {
    return undefined;
  }
}

const id = extensionIdFromManifest();
patchExtensionId(id);
const token = readToken();
if (token && !process.env.PLAYWRIGHT_MCP_EXTENSION_TOKEN)
  process.env.PLAYWRIGHT_MCP_EXTENSION_TOKEN = token;

const args = process.argv.slice(2);
if (!args.includes('--extension'))
  args.unshift('--extension');
if (process.env.JCODE_BROWSER_EXE && !args.includes('--executable-path'))
  args.push('--executable-path', process.env.JCODE_BROWSER_EXE);
if (process.env.JCODE_BROWSER_PROFILE && !args.includes('--user-data-dir'))
  args.push('--user-data-dir', process.env.JCODE_BROWSER_PROFILE);
const cli = path.join(path.dirname(require.resolve('@playwright/mcp/package.json')), 'cli.js');
process.argv = [process.argv[0], cli, ...args];
require(cli);
