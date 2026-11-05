# pcode

A personal build of [Jcode Desktop](https://github.com/1jehuang/jcode-desktop) and the
[jcode](https://github.com/1jehuang/jcode) CLI for Windows. It has these changes:

- **Persian / right-to-left text:** bidi support in a modified GPUI (`gpui/`) and Desktop markdown fixes.
- **Plugins page:** a sidebar entry, plus `/plugins` before draft queueing.
- **Chat layout:** a session pane, context breakdown, background task tray, pinned plan,
  liquid-glass themes, a floating session pane and full-width lone chats.
- **Tray:** the app keeps running when the window closes, with a tray icon to reopen or quit.
- **Agent teams:** a read-only group chat; `/team <goal>` makes a chat the manager.
- **Keyless login (work in progress):** the source is in `desktop/crates/jcode-desktop-harness/src/keyless*`,
  but it is *not* in the prebuilt binary yet.
- **Windows overlay** for windows-mcp (`overlay/`), plus the **Jcode Browser Agent** extension and
  its Playwright-MCP launcher (`browser-extension/`, `browser-mcp/`).

## Install (Windows 10/11)

Double-click `INSTALL.bat`, or run:

```powershell
irm https://raw.githubusercontent.com/PEDIHS/pcode/main/install.ps1 | iex
```

The installer sets up Node.js, Python 3.12 and uv if they are missing. It then installs the prebuilt app
from `dist/pcode-windows-x64.zip` (checked by SHA-256): Jcode Desktop 0.6.5 with the changes above, and
jcode CLI `v0.98.1-dev (217c196)`. It also installs the overlay, the browser extension and the MCP launcher,
registers the MCP servers, and adds jcode to PATH and the Desktop and Start menu shortcuts.
No accounts or tokens are included. Sign in inside Jcode after installing.

## Layout

| Path | What it is |
|---|---|
| `desktop/` | Jcode Desktop source (`team` branch on top of `desktop-v0.6.5`, all changes included) |
| `gpui/` | GPUI at `be875c5` with right-to-left text support; `desktop/Cargo.toml` patches to `../gpui` |
| `overlay/` | windows-mcp overlay proxy (Python) |
| `browser-extension/`, `browser-mcp/` | Jcode Browser Agent extension and its MCP launcher |
| `patches/` | `base.diff` and `plugins.patch` from development, plus an example build script |
| `dist/` | Prebuilt Windows x64 binaries used by the installer |

The jcode CLI itself is unmodified upstream commit
[`217c196`](https://github.com/1jehuang/jcode/commit/217c196), so its source is not copied here.

## Build from source

You need Rust (stable), the Visual Studio C++ Build Tools and the Windows 10 SDK (`fxc.exe`):

```bash
cd desktop
export GPUI_FXC_PATH='C:\Program Files (x86)\Windows Kits\10\bin\10.0.26100.0\x64\fxc.exe'
export CARGO_BUILD_RUSTC_WRAPPER=""
cargo build --release --bin jcode-desktop
```

## License

Upstream projects are MIT / Apache-2.0. Their LICENSE files are kept in each folder.
