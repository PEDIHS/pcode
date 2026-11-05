//! Team: a group chat of the manager session and the agents it hired, with
//! the members beside it. A local, read-only panel like the changelog that
//! refreshes from the saved sessions while open.
use super::*;
use crate::team_state;
use jcode_desktop_model::team::{Event, Kind, Member, Status, Target, Team};
use std::time::{SystemTime, UNIX_EPOCH};

#[cfg(target_os = "windows")]
const PROSE_FONT: &str = "Segoe UI";
#[cfg(not(target_os = "windows"))]
const PROSE_FONT: &str = ".SystemUIFont";

thread_local! {
    static REFRESH_SCHEDULED: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// A steady color per character, so each agent keeps its look everywhere.
pub(crate) fn character_color(name: &str) -> gpui::Hsla {
    let hash = name
        .bytes()
        .fold(2166136261u32, |h, b| (h ^ b as u32).wrapping_mul(16777619));
    gpui::hsla((hash % 360) as f32 / 360., 0.62, 0.62, 1.)
}

pub(crate) fn title_case(name: &str) -> String {
    let mut chars = name.chars();
    chars
        .next()
        .map(|first| first.to_uppercase().chain(chars).collect())
        .unwrap_or_default()
}

/// Seconds since the epoch for an RFC 3339 UTC timestamp like
/// `2026-11-04T09:00:00.123Z`.
fn epoch_seconds(timestamp: &str) -> Option<i64> {
    let n = |range: std::ops::Range<usize>| timestamp.get(range)?.parse::<i64>().ok();
    let (y, m, d) = (n(0..4)?, n(5..7)?, n(8..10)?);
    let (hh, mm, ss) = (n(11..13)?, n(14..16)?, n(17..19)?);
    // Days from civil (Howard Hinnant).
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doy = (153 * (m + if m > 2 { -3 } else { 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146097 + doe - 719468;
    Some(days * 86400 + hh * 3600 + mm * 60 + ss)
}

fn ago(timestamp: &str) -> String {
    let Some(then) = epoch_seconds(timestamp) else {
        return String::new();
    };
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64);
    match (now - then).max(0) {
        s if s < 60 => "just now".into(),
        s if s < 3600 => format!("{}m ago", s / 60),
        s if s < 86400 => format!("{}h ago", s / 3600),
        s => format!("{}d ago", s / 86400),
    }
}

pub(crate) fn status_color(status: Status) -> gpui::Rgba {
    let theme = Theme::global();
    match status {
        Status::Working => theme.ACCENT,
        Status::Ready => theme.TEXT_DIM,
        Status::Blocked => theme.WARN,
        Status::Done => theme.OK,
        Status::Failed => theme.ERROR,
        Status::Offline => theme.TEXT_FAINT,
    }
}

/// A character portrait: the session's animal on its own tint, with an
/// optional status dot.
pub(crate) fn avatar(name: &str, size: f32, status: Option<Status>) -> gpui::Div {
    let theme = Theme::global();
    let color = character_color(name);
    div()
        .relative()
        .flex_shrink_0()
        .size(px(size))
        .child(
            div()
                .size_full()
                .rounded_full()
                .flex()
                .items_center()
                .justify_center()
                .bg(color.opacity(0.24))
                .border_1()
                .border_color(color.opacity(0.75))
                .text_size(px(size * 0.54))
                .child(jcode_core::id::session_icon(name)),
        )
        .when_some(status, |el, status| {
            let dot = (size * 0.3).max(8.);
            el.child(
                div()
                    .absolute()
                    .bottom(px(-1.))
                    .right(px(-1.))
                    .size(px(dot))
                    .rounded_full()
                    .border_2()
                    .border_color(theme.PANEL_BG)
                    .bg(status_color(status)),
            )
        })
}

/// Overlapping portraits of a team, manager first.
pub(crate) fn avatar_stack(team: &Team, size: f32, max: usize) -> gpui::Div {
    let theme = Theme::global();
    let all: Vec<&Member> = std::iter::once(&team.manager).chain(&team.members).collect();
    let extra = all.len().saturating_sub(max);
    div()
        .flex()
        .items_center()
        .children(all.into_iter().take(max).enumerate().map(move |(i, member)| {
            div()
                .when(i > 0, |el| el.ml(px(-size * 0.32)))
                .rounded_full()
                .border_2()
                .border_color(theme.BG)
                .child(avatar(&member.name, size, None))
        }))
        .when(extra > 0, |el| {
            el.child(
                div()
                    .ml(px(4.))
                    .text_size(px(11.))
                    .text_color(theme.TEXT_DIM)
                    .child(format!("+{extra}")),
            )
        })
}

fn role_of(team: &Team, member: &Member) -> String {
    if member.session_id == team.manager.session_id {
        "Manager".into()
    } else {
        member.role.clone().unwrap_or_else(|| "Agent".into())
    }
}

fn member_row(team: &Team, member: &Member) -> gpui::Div {
    let theme = Theme::global();
    let manager = member.session_id == team.manager.session_id;
    let color = character_color(&member.name);
    div()
        .flex()
        .gap_3()
        .px_3()
        .py(px(10.))
        .rounded_xl()
        .hover(|el| el.bg(theme.HEADER_BG))
        .child(avatar(&member.name, 38., Some(member.status)))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .flex_col()
                .gap(px(2.))
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap_1()
                        .child(
                            div()
                                .font_weight(gpui::FontWeight::SEMIBOLD)
                                .text_size(px(13.))
                                .child(title_case(&member.name)),
                        )
                        .when(manager, |el| el.child(div().text_size(px(12.)).child("👑"))),
                )
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap_2()
                        .text_size(px(11.))
                        .child(
                            div()
                                .px(px(6.))
                                .rounded_full()
                                .bg(color.opacity(0.18))
                                .text_color(theme.TEXT)
                                .child(role_of(team, member)),
                        )
                        .child(div().text_color(status_color(member.status)).child(member.status.label())),
                )
                .when_some(member.task.clone(), |el, task| {
                    let rtl = crate::markdown::starts_rtl(&task);
                    el.child(
                        div()
                            .w_full()
                            .text_size(px(11.))
                            .line_height(px(16.))
                            .text_color(theme.TEXT_DIM)
                            .when(rtl, |el| el.text_right())
                            .child(shorten(&task, 140)),
                    )
                }),
        )
}

pub(crate) fn shorten(text: &str, chars: usize) -> String {
    let text = text.split_whitespace().collect::<Vec<_>>().join(" ");
    match text.char_indices().nth(chars) {
        Some((cut, _)) => format!("{}…", &text[..cut]),
        None => text,
    }
}

fn target_name(team: &Team, target: &Target) -> String {
    match target {
        Target::You => "you".into(),
        Target::Everyone => "everyone".into(),
        Target::Channel(channel) => format!("#{channel}"),
        Target::Member(id) => team
            .member(id)
            .map(|m| title_case(&m.name))
            .unwrap_or_else(|| id.clone()),
    }
}

/// Hiring reads as a quiet notice in the middle of the chat, like a group
/// chat's "X added Y".
fn notice(team: &Team, event: &Event) -> gpui::Div {
    let theme = Theme::global();
    let from = team.member(&event.from).map(|m| m.name.clone()).unwrap_or_default();
    let (role, _) = event.text.split_once('\n').unwrap_or((event.text.as_str(), ""));
    let hired = match &event.to {
        Target::Member(id) => team.member(id).map(|m| m.name.clone()),
        _ => None,
    }
    .unwrap_or_default();
    div().flex().justify_center().child(
        div()
            .flex()
            .items_center()
            .gap_2()
            .px_3()
            .py(px(4.))
            .rounded_full()
            .bg(theme.HEADER_BG)
            .text_size(px(11.))
            .text_color(theme.TEXT_DIM)
            .child(avatar(&from, 18., None))
            .child(format!("{} hired", title_case(&from)))
            .child(avatar(&hired, 18., None))
            .child(
                div()
                    .text_color(theme.TEXT)
                    .child(format!("{} · {role}", title_case(&hired))),
            )
            .child(div().text_color(theme.TEXT_FAINT).child(ago(&event.at))),
    )
}

fn kind_label(kind: Kind) -> &'static str {
    match kind {
        Kind::Prompt => "",
        Kind::Hire => "hired",
        Kind::Task => "📋 task for",
        Kind::Direct => "→",
        Kind::Broadcast => "📣",
        Kind::Channel => "#",
        Kind::Report(Status::Done) => "✅ report to",
        Kind::Report(Status::Blocked) => "⛔ blocked · to",
        Kind::Report(Status::Failed) => "❌ failed · to",
        Kind::Report(_) => "📝 update to",
        Kind::Reply => "→",
    }
}

/// One chat message. `continued` drops the portrait and name when the same
/// sender spoke just before, as group chats do.
fn message(team: &Team, event: &Event, continued: bool) -> gpui::Div {
    let theme = Theme::global();
    let you = event.from.is_empty();
    let sender = team.member(&event.from);
    let name = sender.map(|m| m.name.clone()).unwrap_or_default();
    let tint: gpui::Hsla = if you { theme.USER_ACCENT.into() } else { character_color(&name) };
    let text = shorten_keep_lines(&event.text, 900);
    let rtl = crate::markdown::starts_rtl(&text);
    let header = (!continued).then(|| {
        let target = match event.kind {
            Kind::Broadcast => "everyone".to_string(),
            _ => target_name(team, &event.to),
        };
        div()
            .flex()
            .items_center()
            .gap_2()
            .text_size(px(11.))
            .when(you, |el| el.flex_row_reverse())
            .child(
                div()
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .text_size(px(12.))
                    .text_color(tint)
                    .child(if you { "You".into() } else { title_case(&name) }),
            )
            .when_some(sender.filter(|_| !you), |el, member| {
                el.child(
                    div()
                        .px(px(6.))
                        .rounded_full()
                        .bg(tint.opacity(0.16))
                        .text_color(theme.TEXT_DIM)
                        .child(role_of(team, member)),
                )
            })
            .when(!you, |el| {
                el.child(
                    div()
                        .text_color(theme.TEXT_DIM)
                        .child(format!("{} {target}", kind_label(event.kind))),
                )
            })
            .child(div().text_color(theme.TEXT_FAINT).child(ago(&event.at)))
    });
    div()
        .flex()
        .gap_3()
        .when(you, |el| el.flex_row_reverse())
        .when(continued, |el| el.mt(px(-10.)))
        .child(if continued {
            div().w(px(34.)).flex_shrink_0()
        } else if you {
            div()
                .flex_shrink_0()
                .size(px(34.))
                .rounded_full()
                .flex()
                .items_center()
                .justify_center()
                .bg(theme.USER_BG)
                .border_1()
                .border_color(tint.opacity(0.6))
                .child("🙂")
        } else {
            avatar(&name, 34., None)
        })
        .child(
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .flex_col()
                .gap(px(4.))
                .when(you, |el| el.items_end())
                .children(header)
                .child(
                    div()
                        .w_full()
                        .max_w(px(640.))
                        .px_3()
                        .py_2()
                        .rounded_2xl()
                        .bg(tint.opacity(if you { 0.16 } else { 0.09 }))
                        .border_1()
                        .border_color(tint.opacity(0.22))
                        .text_size(px(13.))
                        .line_height(px(21.))
                        .text_color(theme.TEXT)
                        .when(rtl, |el| el.text_right())
                        .child(text),
                ),
        )
}

fn shorten_keep_lines(text: &str, chars: usize) -> String {
    let text = text.trim();
    match text.char_indices().nth(chars) {
        Some((cut, _)) => format!("{}…", &text[..cut]),
        None => text.to_owned(),
    }
}

impl Panel {
    pub(crate) const TEAM_SESSION_ID: &str = "desktop://team";

    pub(crate) fn is_team(&self) -> bool {
        self.session_id == Self::TEAM_SESSION_ID
    }

    pub(super) fn render_team(&self, _window: &mut Window, cx: &mut Context<Self>) -> gpui::AnyElement {
        let theme = Theme::global();
        // Keep the conversation live while the page is on screen.
        if !REFRESH_SCHEDULED.with(|flag| flag.replace(true)) {
            cx.spawn(async move |this, cx| {
                cx.background_executor().timer(team_state::REFRESH).await;
                REFRESH_SCHEDULED.with(|flag| flag.set(false));
                let _ = this.update(cx, |_, cx| cx.notify());
            })
            .detach();
        }
        let teams = team_state::teams();
        let selected = team_state::selected()
            .and_then(|id| teams.iter().position(|t| t.manager.session_id == id))
            .unwrap_or(0);

        let tabs = div().flex().items_center().gap_1().children(teams.iter().enumerate().map(|(i, team)| {
            let chosen = i == selected;
            let id = team.manager.session_id.clone();
            div()
                .id(("team-tab", i))
                .cursor_pointer()
                .flex()
                .items_center()
                .gap_2()
                .pl_1()
                .pr_3()
                .py(px(3.))
                .rounded_full()
                .text_size(px(12.))
                .text_color(if chosen { theme.TEXT } else { theme.TEXT_DIM })
                .when(chosen, |el| el.bg(theme.HEADER_BG))
                .hover(|el| el.bg(theme.HEADER_BG))
                .on_click(cx.listener(move |_, _, _, cx| {
                    team_state::select(&id);
                    cx.notify();
                }))
                .child(avatar(&team.manager.name, 20., None))
                .child(title_case(&team.manager.name))
        }));

        let close = div()
            .id("close-team")
            .cursor_pointer()
            .px_3()
            .py(px(3.))
            .rounded_full()
            .text_size(px(12.))
            .text_color(theme.TEXT_DIM)
            .hover(|el| el.bg(theme.HEADER_BG).text_color(theme.TEXT))
            .on_click(|_, window, cx| window.dispatch_action(Box::new(crate::ui_actions::ClosePanel), cx))
            .child("Close");

        let body = match teams.get(selected) {
            None => empty_state(),
            Some(team) => {
                let working = team.members.iter().filter(|m| m.status == Status::Working).count();
                let done = team.members.iter().filter(|m| m.status == Status::Done).count();
                let goal = jcode_desktop_model::team::goal(team);
                let rtl = crate::markdown::starts_rtl(&goal);
                let manager_id = team.manager.session_id.clone();
                let open_manager = div()
                    .id("team-open-manager")
                    .flex_shrink_0()
                    .cursor_pointer()
                    .flex()
                    .items_center()
                    .gap_2()
                    .px_3()
                    .py(px(6.))
                    .rounded_full()
                    .bg(character_color(&team.manager.name).opacity(0.22))
                    .border_1()
                    .border_color(character_color(&team.manager.name).opacity(0.6))
                    .text_size(px(12.))
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .hover(|el| el.bg(character_color(&team.manager.name).opacity(0.34)))
                    .on_click(move |_, window, cx| {
                        window.dispatch_action(
                            Box::new(crate::ui_actions::OpenSessionById {
                                session_id: manager_id.clone(),
                            }),
                            cx,
                        )
                    })
                    .child(avatar(&team.manager.name, 20., None))
                    .child(format!("Chat with {}", title_case(&team.manager.name)));
                let hero = div()
                    .flex()
                    .items_center()
                    .gap_3()
                    .px_5()
                    .py_3()
                    .border_b_1()
                    .border_color(theme.PANEL_BORDER)
                    .child(avatar_stack(team, 34., 6))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .flex()
                            .flex_col()
                            .gap(px(2.))
                            .child(
                                div()
                                    .w_full()
                                    .font_weight(gpui::FontWeight::SEMIBOLD)
                                    .text_size(px(15.))
                                    .when(rtl, |el| el.text_right())
                                    .child(shorten(&goal, 120)),
                            )
                            .child(
                                div()
                                    .text_size(px(11.))
                                    .text_color(theme.TEXT_DIM)
                                    .child(format!(
                                        "{} manages {} agent{} · {working} working · {done} done · read-only group, talk to the manager",
                                        title_case(&team.manager.name),
                                        team.members.len(),
                                        if team.members.len() == 1 { "" } else { "s" },
                                    )),
                            ),
                    )
                    .child(open_manager);

                let mut chat = div()
                    .id(("team-chat", selected))
                    .flex_1()
                    .min_w_0()
                    .h_full()
                    .overflow_y_scroll()
                    .px_5()
                    .pt_4()
                    .pb_10()
                    .flex()
                    .flex_col()
                    .gap_4();
                let mut previous: Option<&Event> = None;
                for event in &team.events {
                    if event.kind == Kind::Hire {
                        chat = chat.child(notice(team, event));
                        previous = None;
                        continue;
                    }
                    let continued = previous
                        .is_some_and(|p| p.from == event.from && p.to == event.to && p.kind == event.kind);
                    chat = chat.child(message(team, event, continued));
                    previous = Some(event);
                }

                let members = div()
                    .id("team-members")
                    .w(px(270.))
                    .flex_shrink_0()
                    .h_full()
                    .overflow_y_scroll()
                    .border_l_1()
                    .border_color(theme.PANEL_BORDER)
                    .px_2()
                    .pt_3()
                    .pb_6()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .child(
                        div()
                            .px_3()
                            .pb_1()
                            .text_size(px(11.))
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .text_color(theme.TEXT_DIM)
                            .child(format!("Members · {}", team.members.len() + 1)),
                    )
                    .child(member_row(team, &team.manager))
                    .children(team.members.iter().map(|m| member_row(team, m)));

                div()
                    .flex_1()
                    .min_h_0()
                    .flex()
                    .flex_col()
                    .child(hero)
                    .child(div().flex_1().min_h_0().flex().child(chat).child(members))
            }
        };

        div()
            .id("desktop-team")
            .debug_selector(|| "desktop-team".into())
            .size_full()
            .flex()
            .flex_col()
            .overflow_hidden()
            .font_family(PROSE_FONT)
            .text_size(px(14.))
            .text_color(theme.TEXT)
            .track_focus(&self.focus_handle)
            .on_key_down(cx.listener(|_, event: &gpui::KeyDownEvent, window, cx| {
                if event.keystroke.key == "escape" {
                    window.dispatch_action(Box::new(crate::ui_actions::ClosePanel), cx);
                    cx.stop_propagation();
                }
            }))
            .child(
                div()
                    .px_5()
                    .py_2()
                    .flex_shrink_0()
                    .flex()
                    .items_center()
                    .gap_3()
                    .child(div().text_size(px(16.)).font_weight(gpui::FontWeight::SEMIBOLD).child("Teams"))
                    .child(tabs)
                    .child(div().flex_1())
                    .child(close),
            )
            .child(body)
            .into_any_element()
    }
}

fn empty_state() -> gpui::Div {
    let theme = Theme::global();
    div()
        .flex_1()
        .flex()
        .flex_col()
        .items_center()
        .justify_center()
        .gap_3()
        .px_8()
        .child(div().text_size(px(42.)).child("👥"))
        .child(div().text_size(px(16.)).font_weight(gpui::FontWeight::SEMIBOLD).child("No team yet"))
        .child(
            div()
                .max_w(px(520.))
                .text_center()
                .text_size(px(13.))
                .line_height(px(20.))
                .text_color(theme.TEXT_DIM)
                .child("In any chat, type /team and your goal. That chat becomes the manager: it hires the agents the goal needs, gives each one matching skills, and their conversation shows up here live."),
        )
        .child(
            div()
                .px_4()
                .py_2()
                .rounded_xl()
                .bg(theme.PANEL_BG)
                .border_1()
                .border_color(theme.PANEL_BORDER)
                .text_size(px(13.))
                .child("/team یک لندینگ‌پیج برای فروشگاه قهوه‌ام بساز"),
        )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_utc_timestamps() {
        assert_eq!(epoch_seconds("1970-01-01T00:00:00Z"), Some(0));
        assert_eq!(epoch_seconds("2026-11-04T09:00:00.5Z"), Some(1793782800));
    }
}
