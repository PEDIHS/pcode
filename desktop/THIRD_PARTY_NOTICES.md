# Third-party notices for the pro-features branch

The features added on this branch were written from scratch for Jcode Desktop.
No source code was copied or ported from other projects.

| Feature | Files | Source of the idea |
| --- | --- | --- |
| Session pane (context, plan, background tasks, agents, changed files) | `crates/jcode-desktop-ui/src/panel_pro.rs` | Public behavior of Claude Code and OpenAI Codex (Apache-2.0) desktop UIs; no code used |
| Context breakdown card and compact hint | `panel_pro.rs`, `panel_usage.rs` | Claude Code's `/context` view as documented publicly; data comes from Jcode's own `/context` runtime command |
| Background task tray (live output, elapsed time, stop) | `panel_pro.rs` | Claude Code background tasks as seen by users; reads Jcode's own task files |
| Pinned plan above the input | `panel_pro.rs`, `panel.rs` | Claude Code / Codex todo list as seen by users |
| Chat layout (right-aligned user bubbles, centered column, full-width chats) | `panel_prompt.rs`, `panel.rs`, `panel_composer.rs`, `workspace.rs` | Generic chat-app layout pattern |
| Liquid glass themes (`liquid-glass`, `liquid-glass-light`) | `crates/jcode-desktop-ui-core/src/theme.rs`, `theme/palettes.rs` | Built on Jcode's existing `Glass` preset; design principles from Anthropic's frontend-design skill (guidance only) |
| Plugins page | `panel_plugins.rs`, `jcode-desktop-model/src/plugins.rs` | Merged from the main checkout's `plugins` work |

Claude Code is not open source. Its binaries and code were not opened,
decompiled or copied; only its public documentation and visible behavior
informed these designs. No Anthropic names, logos or assets are used in the UI.
