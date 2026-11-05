//! Composer extras in the spirit of Claude Code and Codex, written from
//! scratch for Jcode Desktop:
//!
//! - The context ring opens a breakdown card (system prompt, built-in tools,
//!   MCP tools, conversation, tool results and files), warns near the limit
//!   and offers `/compact`.
//! - Running background tasks get a tray above the input with elapsed time,
//!   live output tailed from the runtime's task files, and a stop button.
//! - A collapsible right-hand pane gathers context, plan, background
//!   tasks, sub-agents and changed files for the session.
//! - The open plan (todo list) is pinned right above the input with each
//!   step's status.
use super::*;
use std::collections::HashMap;
use std::time::{Duration, Instant};

/// Usage at which the ring and label turn amber and a compact pill appears.
pub(super) const CONTEXT_WARN_PERCENT: f32 = 80.;
/// Usage at which they turn red.
pub(super) const CONTEXT_CRITICAL_PERCENT: f32 = 95.;
const TAIL_BYTES: u64 = 6 * 1024;
const TAIL_LINES: usize = 10;
const BG_POLL: Duration = Duration::from_millis(1000);
/// Width of the centered reading column for messages and the composer.
pub(super) const CHAT_COLUMN: f32 = 780.;
/// Width of the right-hand session pane.
const AGENT_PANE_WIDTH: f32 = 300.;
/// Narrower windows keep the chat full width and show the plan and tasks
/// above the input instead.
const AGENT_PANE_MIN_WINDOW: f32 = 1000.;

#[derive(Default)]
pub(super) struct ProState {
    pub(super) context_open: bool,
    pub(super) context_probe_pending: bool,
    pub(super) breakdown: Option<ContextBreakdown>,
    /// First time each background task id was seen running.
    bg_seen: HashMap<String, Instant>,
    bg_live: HashMap<String, LiveTask>,
    bg_expanded: Option<String>,
    bg_collapsed: bool,
    bg_stop_requested: Vec<String>,
    bg_poll: Option<gpui::Task<()>>,
    plan_collapsed: bool,
    agent_pane_hidden: bool,
    pane_probe_sent: bool,
}

#[derive(Debug, Clone, Default, PartialEq)]
struct LiveTask {
    tail: String,
    pid: Option<u32>,
}

#[derive(Debug, Clone, PartialEq)]
pub(super) struct ContextBreakdown {
    pub(super) window: Option<u64>,
    /// `(label, estimated tokens)` in display order.
    pub(super) rows: Vec<(&'static str, u64)>,
}

fn estimate(text: &str) -> u64 {
    jcode_core::util::estimate_tokens(text) as u64
}

fn estimate_value(value: &serde_json::Value) -> u64 {
    match value {
        serde_json::Value::Null => 0,
        serde_json::Value::String(text) => estimate(text),
        other => estimate(&other.to_string()),
    }
}

fn is_mcp_tool(name: &str) -> bool {
    name.starts_with("mcp__") || name.starts_with("mcp_")
}

/// Split the runtime's `/context` snapshot into display buckets. Counts are
/// character estimates, the same heuristic the TUI uses for tool badges.
pub(super) fn breakdown_from(data: &serde_json::Value) -> ContextBreakdown {
    let system = estimate_value(&data["system_prompt"]["static"])
        + estimate_value(&data["system_prompt"]["dynamic"]);
    let (mut builtin, mut mcp) = (0, 0);
    for tool in data["effective_tools"].as_array().into_iter().flatten() {
        let tokens = estimate_value(tool);
        if tool["name"].as_str().is_some_and(is_mcp_tool) {
            mcp += tokens;
        } else {
            builtin += tokens;
        }
    }
    let (mut messages, mut tool_io) = (0, 0);
    for message in data["messages"].as_array().into_iter().flatten() {
        let role = message["role"].as_str().unwrap_or_default();
        match message["content"].as_array() {
            Some(blocks) if role != "tool" => {
                for block in blocks {
                    let kind = block["type"].as_str().unwrap_or_default();
                    if kind.contains("tool") {
                        tool_io += estimate_value(block);
                    } else {
                        messages += estimate_value(block);
                    }
                }
            }
            _ if role == "tool" => tool_io += estimate_value(&message["content"]),
            _ => messages += estimate_value(&message["content"]),
        }
    }
    ContextBreakdown {
        window: data["context_window"].as_u64().filter(|window| *window > 0),
        rows: vec![
            ("System prompt", system),
            ("Built-in tools", builtin),
            ("MCP tools", mcp),
            ("Messages", messages),
            ("Tool results & files", tool_io),
        ],
    }
}

pub(super) fn context_level_color(percent: Option<f32>, theme: &Theme) -> gpui::Rgba {
    match percent {
        Some(p) if p >= CONTEXT_CRITICAL_PERCENT => theme.ERROR,
        Some(p) if p >= CONTEXT_WARN_PERCENT => theme.WARN,
        _ => theme.TEXT_DIM,
    }
}

fn row_color(index: usize, theme: &Theme) -> gpui::Rgba {
    [
        theme.ACCENT,
        theme.CODE_FUNCTION,
        theme.CODE_KEYWORD,
        theme.OK,
        theme.CODE_STRING,
    ][index % 5]
}

fn format_elapsed(elapsed: Duration) -> String {
    let secs = elapsed.as_secs();
    match secs {
        0..=59 => format!("{secs}s"),
        60..=3599 => format!("{}m {:02}s", secs / 60, secs % 60),
        _ => format!("{}h {:02}m", secs / 3600, (secs / 60) % 60),
    }
}

fn task_dir() -> std::path::PathBuf {
    // Mirrors jcode-base's background task directory.
    std::env::temp_dir().join("jcode-bg-tasks")
}

fn read_live_task(task_id: &str) -> LiveTask {
    use std::io::{Read, Seek, SeekFrom};
    let dir = task_dir();
    let mut tail = String::new();
    if let Ok(mut file) = std::fs::File::open(dir.join(format!("{task_id}.output"))) {
        let len = file.metadata().map(|m| m.len()).unwrap_or(0);
        let _ = file.seek(SeekFrom::Start(len.saturating_sub(TAIL_BYTES)));
        let mut bytes = Vec::new();
        let _ = file.read_to_end(&mut bytes);
        let text = String::from_utf8_lossy(&bytes);
        let lines: Vec<&str> = text
            .lines()
            .map(str::trim_end)
            .filter(|line| !line.is_empty())
            .collect();
        tail = lines[lines.len().saturating_sub(TAIL_LINES)..].join("\n");
    }
    let pid = std::fs::read_to_string(dir.join(format!("{task_id}.status.json")))
        .ok()
        .and_then(|text| serde_json::from_str::<serde_json::Value>(&text).ok())
        .and_then(|status| status["pid"].as_u64())
        .map(|pid| pid as u32);
    LiveTask { tail, pid }
}

fn kill_process_tree(pid: u32) {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        let _ = std::process::Command::new("taskkill")
            .args(["/T", "/F", "/PID", &pid.to_string()])
            .creation_flags(CREATE_NO_WINDOW)
            .spawn();
    }
    #[cfg(not(windows))]
    {
        let _ = std::process::Command::new("kill")
            .args(["-TERM", &format!("-{pid}")])
            .spawn();
    }
}

impl Panel {
    /// Prompt occupancy from the live usage event, or, for a reopened
    /// session that has not run a turn yet, from its latest response stats.
    pub(super) fn context_used(&self) -> Option<u64> {
        self.context_tokens.or_else(|| {
            self.items.iter().rev().find_map(|item| match item {
                Item::ResponseStats(stats) => stats.input_tokens.map(|input| {
                    jcode_base::compaction::effective_context_tokens_from_usage(
                        self.provider.as_deref().unwrap_or_default(),
                        input,
                        stats.cache_read_tokens,
                        stats.cache_creation_tokens,
                    )
                }),
                _ => None,
            })
        })
    }

    pub(super) fn context_percent(&self) -> Option<f32> {
        let window = self
            .model
            .as_deref()
            .and_then(context_window_for_model)
            .or_else(|| self.pro.breakdown.as_ref().and_then(|b| b.window))?;
        self.context_used()
            .map(|used| (used as f64 / window as f64 * 100.) as f32)
    }

    pub(super) fn toggle_context_card(&mut self, cx: &mut Context<Self>) {
        self.pro.context_open = !self.pro.context_open;
        if self.pro.context_open {
            self.request_context_breakdown();
        }
        cx.notify();
    }

    fn request_context_breakdown(&mut self) {
        if self.is_side_document() || self.preview_state.is_some() {
            return;
        }
        self.pro.context_probe_pending = true;
        self.bridge.send(Command::RuntimeCommand {
            session_id: self.session_id.clone(),
            command: jcode_sdk::api::RuntimeCommand::Context,
            args: Vec::new(),
        });
    }

    /// Consume a `/context` reply requested by the card instead of printing
    /// the whole snapshot into the transcript.
    pub(super) fn take_context_probe(
        &mut self,
        command: jcode_sdk::api::RuntimeCommand,
        data: &serde_json::Value,
    ) -> bool {
        if command != jcode_sdk::api::RuntimeCommand::Context || !self.pro.context_probe_pending {
            return false;
        }
        self.pro.context_probe_pending = false;
        self.pro.breakdown = Some(breakdown_from(data));
        true
    }

    pub(super) fn compact_from_card(&mut self, cx: &mut Context<Self>) {
        self.pro.context_open = false;
        self.run_session_operation(SessionOperation::Compact, "Context compacted.");
        self.stick_to_bottom = true;
        cx.notify();
    }

    /// Small amber/red pill next to the ring once the window is nearly full.
    pub(super) fn render_compact_hint(&self, cx: &mut Context<Self>) -> Option<gpui::AnyElement> {
        let percent = self.context_percent()?;
        if percent < CONTEXT_WARN_PERCENT || self.is_side_document() {
            return None;
        }
        let theme = Theme::global();
        let color = context_level_color(Some(percent), &theme);
        Some(
            div()
                .id("panel-compact-hint")
                .debug_selector(|| "panel-compact-hint".into())
                .flex_none()
                .h(px(20.))
                .px_2()
                .flex()
                .items_center()
                .rounded_full()
                .cursor_pointer()
                .bg(gpui::Rgba { a: 0.14, ..color })
                .text_color(color)
                .text_size(px(10.5))
                .font_family(theme.FONT_MONO)
                .child(format!("{:.0}% left · compact", (100. - percent).max(0.)))
                .on_click(cx.listener(|this, _, _, cx| {
                    this.compact_from_card(cx);
                    cx.stop_propagation();
                }))
                .into_any_element(),
        )
    }

    pub(super) fn render_context_card(&self, cx: &mut Context<Self>) -> Option<gpui::AnyElement> {
        if !self.pro.context_open {
            return None;
        }
        let theme = Theme::global();
        let breakdown = self.pro.breakdown.clone();
        let window = self
            .model
            .as_deref()
            .and_then(context_window_for_model)
            .or_else(|| breakdown.as_ref().and_then(|b| b.window));
        let estimated: u64 = breakdown
            .as_ref()
            .map(|b| b.rows.iter().map(|(_, n)| n).sum())
            .unwrap_or(0);
        let used = self.context_used().unwrap_or(estimated);
        let percent = window.map(|window| (used as f64 / window as f64 * 100.) as f32);
        let level = context_level_color(percent, &theme);
        let headline = match (window, percent) {
            (Some(window), Some(percent)) => format!(
                "{} / {} tokens · {percent:.0}%",
                format_tokens(used),
                format_tokens(window)
            ),
            _ => format!("{} tokens", format_tokens(used)),
        };
        // Stacked bar: each bucket's share of the window, free space as track.
        let scale = window.unwrap_or(estimated.max(1)).max(1) as f32;
        let mut bar = div()
            .w_full()
            .h(px(8.))
            .rounded_full()
            .overflow_hidden()
            .flex()
            .bg(theme.INLINE_CODE_BG);
        let mut rows = div().flex().flex_col().gap(px(3.));
        match &breakdown {
            Some(breakdown) => {
                for (index, (label, tokens)) in breakdown.rows.iter().enumerate() {
                    let color = row_color(index, &theme);
                    if *tokens > 0 {
                        bar = bar.child(
                            div()
                                .h_full()
                                .w(relative((*tokens as f32 / scale).clamp(0., 1.)))
                                .bg(color),
                        );
                    }
                    let share = window
                        .map(|w| format!("{:.1}%", *tokens as f64 / w as f64 * 100.))
                        .unwrap_or_default();
                    rows = rows.child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .text_size(px(11.5))
                            .child(div().flex_none().size(px(8.)).rounded_sm().bg(color))
                            .child(div().flex_1().text_color(theme.TEXT).child(*label))
                            .child(
                                div()
                                    .font_family(theme.FONT_MONO)
                                    .text_color(theme.TEXT_DIM)
                                    .child(format!("~{}", format_tokens(*tokens))),
                            )
                            .child(
                                div()
                                    .w(px(44.))
                                    .flex()
                                    .justify_end()
                                    .font_family(theme.FONT_MONO)
                                    .text_color(theme.TEXT_FAINT)
                                    .child(share),
                            ),
                    );
                }
                if let Some(window) = window {
                    rows = rows.child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .text_size(px(11.5))
                            .child(
                                div()
                                    .flex_none()
                                    .size(px(8.))
                                    .rounded_sm()
                                    .border_1()
                                    .border_color(theme.TOOL_BORDER),
                            )
                            .child(div().flex_1().text_color(theme.TEXT_DIM).child("Free space"))
                            .child(
                                div()
                                    .font_family(theme.FONT_MONO)
                                    .text_color(theme.TEXT_DIM)
                                    .child(format_tokens(window.saturating_sub(used))),
                            )
                            .child(div().w(px(44.))),
                    );
                }
            }
            None => {
                rows = rows.child(
                    div()
                        .text_size(px(11.5))
                        .text_color(theme.TEXT_DIM)
                        .child(if self.pro.context_probe_pending {
                            "Measuring context…"
                        } else {
                            "No breakdown yet."
                        }),
                );
            }
        }
        let warning = percent.filter(|p| *p >= CONTEXT_WARN_PERCENT).map(|p| {
            div()
                .text_size(px(11.))
                .text_color(level)
                .child(if p >= CONTEXT_CRITICAL_PERCENT {
                    "Context is almost full. Compact now to keep the conversation going."
                } else {
                    "Context is filling up. Consider compacting soon."
                })
        });
        let button = |id: &'static str, label: &'static str, primary: bool| {
            div()
                .id(id)
                .debug_selector(move || id.into())
                .h(px(22.))
                .px_2p5()
                .flex()
                .items_center()
                .rounded_md()
                .cursor_pointer()
                .text_size(px(11.))
                .when(primary, |el| el.bg(theme.ACCENT).text_color(theme.BG))
                .when(!primary, |el| {
                    el.border_1()
                        .border_color(theme.TOOL_BORDER)
                        .text_color(theme.TEXT_DIM)
                })
                .child(label)
        };
        Some(
            div()
                .id("context-card")
                .debug_selector(|| "context-card".into())
                .mx_1()
                .mb(px(6.))
                .p_3()
                .flex()
                .flex_col()
                .gap_2()
                .rounded(px(14.))
                .bg(theme.HEADER_BG)
                .border_1()
                .border_color(theme.PANEL_BORDER)
                .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| cx.stop_propagation())
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap_2()
                        .child(
                            div()
                                .flex_1()
                                .text_size(px(12.5))
                                .font_weight(gpui::FontWeight::MEDIUM)
                                .text_color(theme.TEXT)
                                .child("Context window"),
                        )
                        .child(
                            div()
                                .text_size(px(11.))
                                .font_family(theme.FONT_MONO)
                                .text_color(level)
                                .child(headline),
                        )
                        .child(
                            div()
                                .id("context-card-close")
                                .px_1()
                                .cursor_pointer()
                                .text_color(theme.TEXT_FAINT)
                                .child("×")
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.pro.context_open = false;
                                    cx.notify();
                                })),
                        ),
                )
                .child(bar)
                .child(rows)
                .children(warning)
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap_2()
                        .child(
                            div()
                                .flex_1()
                                .text_size(px(10.))
                                .text_color(theme.TEXT_FAINT)
                                .child("Buckets are estimates from the runtime's /context snapshot. The total is the latest provider-reported request."),
                        )
                        .child(button("context-card-refresh", "Refresh", false).on_click(
                            cx.listener(|this, _, _, cx| {
                                this.request_context_breakdown();
                                cx.notify();
                            }),
                        ))
                        .child(button("context-card-compact", "Compact now", true).on_click(
                            cx.listener(|this, _, _, cx| this.compact_from_card(cx)),
                        )),
                )
                .into_any_element(),
        )
    }

    /// The latest plan while any step is still open. Finished plans fall
    /// back to the pinned card above the transcript.
    pub(super) fn active_plan(&self) -> Option<TodoCardPayload> {
        self.latest_todo_payload().filter(|payload| {
            payload
                .todos
                .iter()
                .any(|todo| !matches!(todo.status.as_str(), "completed" | "cancelled"))
        })
    }

    /// Live plan pinned directly above the input, under the latest message.
    pub(super) fn render_plan_strip(&self, cx: &mut Context<Self>) -> Option<gpui::AnyElement> {
        if self.is_side_document() || self.transcript_only {
            return None;
        }
        let payload = self.active_plan()?;
        let theme = Theme::global();
        let total = payload.todos.len();
        let done = payload
            .todos
            .iter()
            .filter(|todo| todo.status == "completed")
            .count();
        let current = payload
            .todos
            .iter()
            .find(|todo| todo.status == "in_progress")
            .or_else(|| payload.todos.iter().find(|todo| todo.status == "pending"))
            .map(|todo| todo.content.clone())
            .unwrap_or_default();
        let collapsed = self.pro.plan_collapsed;
        let header = div()
            .id("plan-strip-header")
            .debug_selector(|| "plan-strip-header".into())
            .flex()
            .items_center()
            .gap_2()
            .h(px(22.))
            .min_w_0()
            .cursor_pointer()
            .text_size(px(11.))
            .child(
                div()
                    .flex_none()
                    .font_weight(gpui::FontWeight::MEDIUM)
                    .text_color(theme.TEXT)
                    .child("Plan"),
            )
            .child(
                div()
                    .flex_none()
                    .font_family(theme.FONT_MONO)
                    .text_color(theme.TEXT_DIM)
                    .child(format!("{done}/{total}")),
            )
            .child(
                div()
                    .flex_none()
                    .w(px(56.))
                    .h(px(3.))
                    .rounded_full()
                    .bg(theme.TOOL_BORDER)
                    .child(
                        div()
                            .h_full()
                            .w(relative(done as f32 / total.max(1) as f32))
                            .rounded_full()
                            .bg(theme.OK),
                    ),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_color(theme.TEXT_DIM)
                    .when(collapsed, |el| el.child(current)),
            )
            .child(
                div()
                    .flex_none()
                    .text_color(theme.TEXT_FAINT)
                    .child(if collapsed { "show ▴" } else { "hide ▾" }),
            )
            .on_click(cx.listener(|this, _, _, cx| {
                this.pro.plan_collapsed = !this.pro.plan_collapsed;
                cx.notify();
            }));
        let mut list = div()
            .id("plan-strip-list")
            .flex()
            .flex_col()
            .gap(px(1.))
            .max_h(px(168.))
            .overflow_y_scroll();
        if !collapsed {
            for todo in &payload.todos {
                let (glyph, glyph_color, text_color) = match todo.status.as_str() {
                    "completed" => ("✓", theme.OK, theme.TEXT_FAINT),
                    "in_progress" => ("◉", theme.ACCENT, theme.TEXT),
                    "cancelled" => ("✕", theme.TEXT_FAINT, theme.TEXT_FAINT),
                    _ if !todo.blocked_by.is_empty() => ("◌", theme.WARN, theme.TEXT_DIM),
                    _ => ("○", theme.TEXT_FAINT, theme.TEXT_DIM),
                };
                list = list.child(
                    div()
                        .flex()
                        .items_start()
                        .gap_2()
                        .min_w_0()
                        .text_size(px(11.5))
                        .child(div().flex_none().w(px(12.)).text_color(glyph_color).child(glyph))
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .text_color(text_color)
                                .when(todo.status == "in_progress", |el| {
                                    el.font_weight(gpui::FontWeight::MEDIUM)
                                })
                                .child(todo.content.clone()),
                        ),
                );
            }
        }
        Some(
            div()
                .debug_selector(|| "plan-strip".into())
                .mx_1()
                .mb(px(6.))
                .px_3()
                .py_1p5()
                .flex()
                .flex_col()
                .gap_1()
                .rounded(px(14.))
                .bg(theme.CODE_BG)
                .border_1()
                .border_color(theme.TOOL_BORDER)
                .child(header)
                .when(!collapsed, |el| el.child(list))
                .into_any_element(),
        )
    }

}

const AGENT_TOOLS: &[&str] = &["subagent", "task", "task_runner", "swarm", "agent"];
const EDIT_TOOLS: &[&str] = &[
    "edit", "write", "multiedit", "multi_edit", "patch", "apply_patch", "str_replace", "create",
];

fn input_field(input: &str, keys: &[&str]) -> Option<String> {
    let value: serde_json::Value = serde_json::from_str(input).ok()?;
    keys.iter()
        .find_map(|key| value[*key].as_str().map(str::trim).filter(|v| !v.is_empty()))
        .map(|text| text.lines().next().unwrap_or(text).to_string())
}

fn short_path(path: &str) -> String {
    let parts: Vec<&str> = path.split(['/', '\\']).filter(|p| !p.is_empty()).collect();
    match parts.len() {
        0 => path.to_string(),
        1 | 2 => parts.join("/"),
        n => format!("…/{}/{}", parts[n - 2], parts[n - 1]),
    }
}

fn pane_section(title: &'static str, count: Option<usize>) -> gpui::Div {
    let theme = Theme::global();
    div()
        .flex()
        .items_center()
        .gap_2()
        .pt_3()
        .pb_1()
        .text_size(px(10.5))
        .font_weight(gpui::FontWeight::SEMIBOLD)
        .text_color(theme.TEXT_FAINT)
        .child(title.to_uppercase())
        .children(count.map(|count| {
            div()
                .px_1p5()
                .rounded_full()
                .bg(theme.INLINE_CODE_BG)
                .text_color(theme.TEXT_DIM)
                .child(count.to_string())
        }))
}

fn pane_empty(text: &'static str) -> gpui::Div {
    div()
        .py_1()
        .text_size(px(11.5))
        .text_color(Theme::global().TEXT_FAINT)
        .child(text)
}

impl Panel {
    pub(super) fn agent_pane_visible(&self, window: &Window) -> bool {
        !self.pro.agent_pane_hidden
            && !self.transcript_only
            && !self.is_side_document()
            && !self.image_pane_open
            && self.preview_state.is_none()
            && self.orchestration.is_none()
            && f32::from(window.viewport_size().width) >= AGENT_PANE_MIN_WINDOW
    }

    pub(super) fn render_agent_pane_toggle(
        &self,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> Option<gpui::AnyElement> {
        if self.transcript_only || self.is_side_document() || self.image_pane_open {
            return None;
        }
        if f32::from(window.viewport_size().width) < AGENT_PANE_MIN_WINDOW {
            return None;
        }
        let theme = Theme::global();
        let open = !self.pro.agent_pane_hidden;
        Some(
            div()
                .id("agent-pane-toggle")
                .debug_selector(|| "agent-pane-toggle".into())
                .flex_none()
                .h(px(22.))
                .px_2()
                .flex()
                .items_center()
                .rounded_full()
                .cursor_pointer()
                .text_size(px(11.))
                .text_color(if open { theme.ACCENT } else { theme.TEXT_DIM })
                .when(open, |el| el.bg(theme.ACCENT_DIM))
                .hover(|el| el.text_color(theme.TEXT))
                .tooltip(|_, cx| {
                    cx.new(|_| super::usage::MeterTooltip("Toggle the session panel".into()))
                        .into()
                })
                .child("▥ panel")
                .on_click(cx.listener(|this, _, _, cx| {
                    this.pro.agent_pane_hidden = !this.pro.agent_pane_hidden;
                    cx.notify();
                    cx.stop_propagation();
                }))
                .into_any_element(),
        )
    }

    fn render_pane_context(&mut self, cx: &mut Context<Self>) -> gpui::Div {
        let theme = Theme::global();
        if self.pro.breakdown.is_none()
            && !self.pro.pane_probe_sent
            && self.model.is_some()
            && !self.demo
        {
            self.pro.pane_probe_sent = true;
            self.request_context_breakdown();
        }
        let percent = self.context_percent();
        let level = context_level_color(percent, &theme);
        let window = self
            .model
            .as_deref()
            .and_then(context_window_for_model)
            .or_else(|| self.pro.breakdown.as_ref().and_then(|b| b.window));
        let used = self.context_used();
        let mut section = div().flex().flex_col().gap_1p5().child(pane_section("Context", None));
        let headline = match (used, window) {
            (Some(used), Some(window)) => format!(
                "{} of {} tokens",
                format_tokens(used),
                format_tokens(window)
            ),
            (Some(used), None) => format!("{} tokens", format_tokens(used)),
            _ => "Not reported yet".to_string(),
        };
        section = section.child(
            div()
                .flex()
                .items_baseline()
                .gap_2()
                .child(
                    div()
                        .text_size(px(20.))
                        .font_weight(gpui::FontWeight::SEMIBOLD)
                        .text_color(level)
                        .child(
                            percent
                                .map(|p| format!("{:.0}%", p.min(100.)))
                                .unwrap_or_else(|| "—".into()),
                        ),
                )
                .child(div().text_size(px(11.)).text_color(theme.TEXT_DIM).child(headline)),
        );
        let scale = window.unwrap_or(1).max(1) as f32;
        let mut bar = div()
            .w_full()
            .h(px(6.))
            .rounded_full()
            .overflow_hidden()
            .flex()
            .bg(theme.INLINE_CODE_BG);
        match &self.pro.breakdown {
            Some(breakdown) if window.is_some() => {
                for (index, (_, tokens)) in breakdown.rows.iter().enumerate() {
                    bar = bar.child(
                        div()
                            .h_full()
                            .w(relative((*tokens as f32 / scale).clamp(0., 1.)))
                            .bg(row_color(index, &theme)),
                    );
                }
            }
            _ => {
                bar = bar.child(
                    div()
                        .h_full()
                        .w(relative(percent.unwrap_or(0.).clamp(0., 100.) / 100.))
                        .bg(level),
                );
            }
        }
        section = section.child(bar);
        if let Some(breakdown) = &self.pro.breakdown {
            for (index, (label, tokens)) in breakdown.rows.iter().enumerate() {
                section = section.child(
                    div()
                        .flex()
                        .items_center()
                        .gap_2()
                        .text_size(px(11.))
                        .child(
                            div()
                                .flex_none()
                                .size(px(7.))
                                .rounded_sm()
                                .bg(row_color(index, &theme)),
                        )
                        .child(div().flex_1().text_color(theme.TEXT_DIM).child(*label))
                        .child(
                            div()
                                .font_family(theme.FONT_MONO)
                                .text_color(theme.TEXT_FAINT)
                                .child(format!("~{}", format_tokens(*tokens))),
                        ),
                );
            }
        }
        let action = |id: &'static str, label: &'static str| {
            div()
                .id(id)
                .debug_selector(move || id.into())
                .h(px(22.))
                .px_2p5()
                .flex()
                .items_center()
                .rounded_md()
                .cursor_pointer()
                .border_1()
                .border_color(theme.TOOL_BORDER)
                .text_size(px(11.))
                .text_color(theme.TEXT_DIM)
                .hover(|el| el.text_color(theme.TEXT))
                .child(label)
        };
        section.child(
            div()
                .flex()
                .gap_2()
                .pt_1()
                .child(action("pane-context-refresh", "Refresh").on_click(cx.listener(
                    |this, _, _, cx| {
                        this.request_context_breakdown();
                        cx.notify();
                    },
                )))
                .child(
                    action("pane-context-compact", "Compact")
                        .on_click(cx.listener(|this, _, _, cx| this.compact_from_card(cx))),
                ),
        )
    }

    fn render_pane_agents(&self) -> gpui::Div {
        let theme = Theme::global();
        let mut rows = Vec::new();
        let mut files: Vec<(String, usize)> = Vec::new();
        for item in self.items.iter() {
            let Item::Tool {
                name,
                input,
                done,
                error,
                ..
            } = item
            else {
                continue;
            };
            let lower = name.to_ascii_lowercase();
            if AGENT_TOOLS.contains(&lower.as_str()) {
                let title = input_field(input, &["description", "title", "task", "prompt", "name"])
                    .unwrap_or_else(|| name.clone());
                rows.push((title, *done, error.is_some()));
            } else if EDIT_TOOLS.contains(&lower.as_str())
                && let Some(path) = input_field(input, &["file_path", "path", "filename"])
            {
                match files.iter_mut().find(|(known, _)| *known == path) {
                    Some((_, count)) => *count += 1,
                    None => files.push((path, 1)),
                }
            }
        }
        let mut agents = div()
            .flex()
            .flex_col()
            .gap_1()
            .child(pane_section("Agents", Some(rows.len())));
        if rows.is_empty() {
            agents = agents.child(pane_empty("No sub-agents in this session."));
        }
        for (title, done, failed) in rows.into_iter().rev().take(8) {
            let (glyph, color) = match (done, failed) {
                (_, true) => ("✕", theme.ERROR),
                (true, _) => ("✓", theme.OK),
                _ => ("◉", theme.WARN),
            };
            agents = agents.child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .min_w_0()
                    .text_size(px(11.5))
                    .child(div().flex_none().w(px(12.)).text_color(color).child(glyph))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_color(theme.TEXT_DIM)
                            .child(title),
                    ),
            );
        }
        agents = agents.child(pane_section("Changed files", Some(files.len())));
        if files.is_empty() {
            agents = agents.child(pane_empty("No file edits yet."));
        }
        for (path, count) in files.into_iter().rev().take(12) {
            agents = agents.child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .min_w_0()
                    .text_size(px(11.))
                    .font_family(theme.FONT_MONO)
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_color(theme.TEXT_DIM)
                            .child(short_path(&path)),
                    )
                    .when(count > 1, |el| {
                        el.child(div().text_color(theme.TEXT_FAINT).child(format!("×{count}")))
                    }),
            );
        }
        agents
    }

    pub(super) fn render_agent_pane(&mut self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let theme = Theme::global();
        let context = self.render_pane_context(cx);
        let plan = self.render_plan_strip(cx);
        let tasks = self.render_background_tray(cx);
        let agents = self.render_pane_agents();
        let header = div()
            .flex()
            .items_center()
            .h(px(36.))
            .child(
                div()
                    .flex_1()
                    .text_size(px(13.))
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .text_color(theme.TEXT)
                    .child("Session"),
            )
            .child(
                div()
                    .id("agent-pane-close")
                    .px_1p5()
                    .rounded_sm()
                    .cursor_pointer()
                    .text_color(theme.TEXT_FAINT)
                    .hover(|el| el.text_color(theme.TEXT))
                    .child("×")
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.pro.agent_pane_hidden = true;
                        cx.notify();
                    })),
            );
        div()
            .id("agent-pane")
            .debug_selector(|| "agent-pane".into())
            .flex_none()
            .w(px(AGENT_PANE_WIDTH))
            // A floating glass card beside the chat rather than a docked rail.
            .my_2()
            .mr_2()
            .px_4()
            .pb_4()
            .flex()
            .flex_col()
            .overflow_y_scroll()
            .rounded(px(16.))
            .bg(theme.HEADER_BG)
            .border_1()
            .border_color(theme.PANEL_BORDER)
            .shadow_lg()
            .child(header)
            .child(context)
            .child(pane_section("Plan", None))
            .child(plan.unwrap_or_else(|| pane_empty("No open plan.").into_any_element()))
            .child(pane_section("Background tasks", None))
            .child(tasks.unwrap_or_else(|| {
                pane_empty("Nothing running in the background.").into_any_element()
            }))
            .child(agents)
            .into_any_element()
    }
}

impl Panel {
    /// Latest state of every background task still running, oldest first.
    fn running_background_tasks(&self) -> Vec<(String, String, String, Option<f32>)> {
        let mut latest: Vec<(String, String, String, Option<f32>, bool)> = Vec::new();
        for item in self.items.iter() {
            if let Item::BackgroundTask {
                task_id,
                label,
                summary,
                percent,
                done,
            } = item
            {
                match latest.iter_mut().find(|row| row.0 == *task_id) {
                    Some(row) => {
                        *row = (task_id.clone(), label.clone(), summary.clone(), *percent, *done)
                    }
                    None => latest.push((
                        task_id.clone(),
                        label.clone(),
                        summary.clone(),
                        *percent,
                        *done,
                    )),
                }
            }
        }
        latest
            .into_iter()
            .filter(|row| !row.4)
            .map(|(id, label, summary, percent, _)| (id, label, summary, percent))
            .collect()
    }

    fn ensure_background_poll(&mut self, cx: &mut Context<Self>) {
        if self.pro.bg_poll.is_some()
            || cfg!(test)
            || crate::harness::screenshot_mode()
            || crate::harness::remote_host(&self.session_id).is_some()
        {
            return;
        }
        self.pro.bg_poll = Some(cx.spawn(async move |this, cx| {
            loop {
                let Ok(ids) = this.update(cx, |panel, _| {
                    panel
                        .running_background_tasks()
                        .into_iter()
                        .map(|row| row.0)
                        .collect::<Vec<_>>()
                }) else {
                    break;
                };
                if ids.is_empty() {
                    let _ = this.update(cx, |panel, _| panel.pro.bg_poll = None);
                    break;
                }
                let live = cx
                    .background_executor()
                    .spawn(async move {
                        ids.into_iter()
                            .map(|id| {
                                let task = read_live_task(&id);
                                (id, task)
                            })
                            .collect::<HashMap<_, _>>()
                    })
                    .await;
                if this
                    .update(cx, |panel, cx| {
                        panel.pro.bg_live = live;
                        // Elapsed time ticks even when the output is quiet.
                        cx.notify();
                    })
                    .is_err()
                {
                    break;
                }
                cx.background_executor().timer(BG_POLL).await;
            }
        }));
    }

    fn stop_background_task(&mut self, task_id: &str, cx: &mut Context<Self>) {
        if self.pro.bg_stop_requested.iter().any(|id| id == task_id) {
            return;
        }
        self.pro.bg_stop_requested.push(task_id.to_string());
        match self.pro.bg_live.get(task_id).and_then(|live| live.pid) {
            Some(pid) => kill_process_tree(pid),
            // In-process tasks have no pid the desktop can signal. Ask the
            // agent, which owns the task, to cancel it with its `bg` tool.
            None => self.submit_or_queue(
                format!(
                    "Cancel background task `{task_id}` now with the bg tool (action cancel) and confirm in one line."
                ),
                Vec::new(),
                true,
                cx,
            ),
        }
        cx.notify();
    }

    pub(super) fn render_background_tray(
        &mut self,
        cx: &mut Context<Self>,
    ) -> Option<gpui::AnyElement> {
        if self.is_side_document() {
            return None;
        }
        let tasks = self.running_background_tasks();
        let now = Instant::now();
        self.pro
            .bg_seen
            .retain(|id, _| tasks.iter().any(|task| task.0 == *id));
        if tasks.is_empty() {
            return None;
        }
        for task in &tasks {
            self.pro.bg_seen.entry(task.0.clone()).or_insert(now);
        }
        self.ensure_background_poll(cx);
        let theme = Theme::global();
        let collapsed = self.pro.bg_collapsed;
        let header = div()
            .id("bg-tray-header")
            .debug_selector(|| "bg-tray-header".into())
            .flex()
            .items_center()
            .gap_2()
            .h(px(22.))
            .cursor_pointer()
            .text_size(px(11.))
            .child(div().text_color(theme.WARN).child("●"))
            .child(
                div()
                    .flex_1()
                    .text_color(theme.TEXT)
                    .font_weight(gpui::FontWeight::MEDIUM)
                    .child(format!(
                        "{} background task{} running",
                        tasks.len(),
                        if tasks.len() == 1 { "" } else { "s" }
                    )),
            )
            .child(
                div()
                    .text_color(theme.TEXT_FAINT)
                    .child(if collapsed { "show ▴" } else { "hide ▾" }),
            )
            .on_click(cx.listener(|this, _, _, cx| {
                this.pro.bg_collapsed = !this.pro.bg_collapsed;
                cx.notify();
            }));
        let mut tray = div()
            .debug_selector(|| "bg-tray".into())
            .mx_1()
            .mb(px(6.))
            .px_3()
            .py_1p5()
            .flex()
            .flex_col()
            .gap_1()
            .rounded(px(14.))
            .bg(theme.TOOL_BG)
            .border_1()
            .border_color(theme.PANEL_BORDER)
            .child(header);
        if collapsed {
            return Some(tray.into_any_element());
        }
        for (index, (task_id, label, summary, percent)) in tasks.into_iter().enumerate() {
            let elapsed = self
                .pro
                .bg_seen
                .get(&task_id)
                .map(|start| format_elapsed(now.duration_since(*start)))
                .unwrap_or_default();
            let live = self.pro.bg_live.get(&task_id).cloned().unwrap_or_default();
            let expanded = self.pro.bg_expanded.as_deref() == Some(task_id.as_str());
            let stopping = self.pro.bg_stop_requested.iter().any(|id| *id == task_id);
            let last_line = live.tail.lines().last().unwrap_or(summary.as_str()).to_string();
            let toggle_id = task_id.clone();
            let stop_id = task_id.clone();
            let row = div()
                .flex()
                .items_center()
                .gap_2()
                .min_w_0()
                .h(px(22.))
                .text_size(px(11.))
                .child(crate::tool_icon::render("bg"))
                .child(
                    div()
                        .flex_none()
                        .max_w(px(160.))
                        .truncate()
                        .text_color(theme.TOOL_TEXT)
                        .child(label),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .font_family(theme.FONT_MONO)
                        .text_size(px(10.5))
                        .text_color(theme.TEXT_DIM)
                        .child(last_line),
                )
                .when_some(percent.filter(|p| p.is_finite()), |el, percent| {
                    el.child(
                        div()
                            .flex_none()
                            .w(px(40.))
                            .h(px(3.))
                            .rounded_full()
                            .bg(theme.TOOL_BORDER)
                            .child(
                                div()
                                    .h_full()
                                    .w(relative((percent / 100.).clamp(0., 1.)))
                                    .rounded_full()
                                    .bg(theme.ACCENT),
                            ),
                    )
                })
                .child(
                    div()
                        .flex_none()
                        .font_family(theme.FONT_MONO)
                        .text_size(px(10.5))
                        .text_color(theme.TEXT_FAINT)
                        .child(elapsed),
                )
                .child(
                    div()
                        .id(("bg-tray-output", index))
                        .flex_none()
                        .px_1p5()
                        .rounded_sm()
                        .cursor_pointer()
                        .text_color(if expanded { theme.ACCENT } else { theme.TEXT_DIM })
                        .child("output")
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.pro.bg_expanded = if this.pro.bg_expanded.as_deref()
                                == Some(toggle_id.as_str())
                            {
                                None
                            } else {
                                Some(toggle_id.clone())
                            };
                            cx.notify();
                            cx.stop_propagation();
                        })),
                )
                .child(
                    div()
                        .id(("bg-tray-stop", index))
                        .debug_selector(|| "bg-tray-stop".into())
                        .flex_none()
                        .px_1p5()
                        .rounded_sm()
                        .cursor_pointer()
                        .text_color(if stopping { theme.TEXT_FAINT } else { theme.ERROR })
                        .child(if stopping { "stopping…" } else { "■ stop" })
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.stop_background_task(&stop_id, cx);
                            cx.stop_propagation();
                        })),
                );
            tray = tray.child(row);
            if expanded {
                let body = if live.tail.is_empty() {
                    "No output yet.".to_string()
                } else {
                    live.tail.clone()
                };
                tray = tray.child(
                    div()
                        .ml(px(22.))
                        .p_2()
                        .rounded_md()
                        .bg(theme.INLINE_CODE_BG)
                        .font_family(theme.FONT_MONO)
                        .text_size(px(10.5))
                        .text_color(theme.CODE_TEXT)
                        .child(body)
                        .child(
                            div()
                                .pt_1()
                                .text_color(theme.TEXT_FAINT)
                                .child(format!(
                                    "task {task_id}{}",
                                    live.pid.map(|pid| format!(" · pid {pid}")).unwrap_or_default()
                                )),
                        ),
                );
            }
        }
        Some(tray.into_any_element())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn breakdown_splits_tools_and_messages() {
        let data = serde_json::json!({
            "context_window": 200000,
            "system_prompt": {"static": "x".repeat(400), "dynamic": ""},
            "effective_tools": [
                {"name": "bash", "description": "y".repeat(400)},
                {"name": "mcp__playwright__click", "description": "z".repeat(800)}
            ],
            "messages": [
                {"role": "user", "content": [{"type": "text", "text": "hello there"}]},
                {"role": "assistant", "content": [{"type": "tool_use", "name": "read"}]},
                {"role": "user", "content": [{"type": "tool_result", "content": "a".repeat(4000)}]}
            ]
        });
        let breakdown = breakdown_from(&data);
        assert_eq!(breakdown.window, Some(200_000));
        let get = |label| breakdown.rows.iter().find(|r| r.0 == label).unwrap().1;
        assert!(get("System prompt") >= 100);
        assert!(get("MCP tools") > get("Built-in tools"));
        assert!(get("Tool results & files") > get("Messages"));
    }

    #[test]
    fn elapsed_is_compact() {
        assert_eq!(format_elapsed(Duration::from_secs(5)), "5s");
        assert_eq!(format_elapsed(Duration::from_secs(75)), "1m 15s");
        assert_eq!(format_elapsed(Duration::from_secs(3700)), "1h 01m");
    }
}
