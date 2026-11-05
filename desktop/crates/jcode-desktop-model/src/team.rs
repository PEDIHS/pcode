//! Agent teams read from saved sessions: a manager session, the agents it
//! hired with the `swarm` tool, and everything they said to each other.
//!
//! Sessions live in `~/.jcode/sessions/` as a snapshot (`<id>.json`) plus a
//! journal of messages appended since (`<id>.journal.jsonl`). A team is a
//! session that called `swarm` with `action: "spawn"`; the spawn result names
//! each new agent's session id.

use serde_json::Value;
use std::collections::{BTreeMap, HashMap};
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Status {
    Working,
    Ready,
    Blocked,
    Done,
    Failed,
    Offline,
}

impl Status {
    pub fn label(self) -> &'static str {
        match self {
            Self::Working => "working",
            Self::Ready => "ready",
            Self::Blocked => "blocked",
            Self::Done => "done",
            Self::Failed => "failed",
            Self::Offline => "offline",
        }
    }

    fn from_report(status: &str) -> Option<Self> {
        Some(match status.to_ascii_lowercase().as_str() {
            "running" | "working" | "in_progress" => Self::Working,
            "ready" | "idle" => Self::Ready,
            "blocked" => Self::Blocked,
            "completed" | "complete" | "done" => Self::Done,
            "failed" | "error" => Self::Failed,
            _ => return None,
        })
    }
}

#[derive(Clone, Debug)]
pub struct Member {
    pub session_id: String,
    /// The session's friendly name, such as `fox`. Its character.
    pub name: String,
    /// The role it was hired for (the spawn label), `None` for the manager.
    pub role: Option<String>,
    pub task: Option<String>,
    pub status: Status,
    pub last_active: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Target {
    You,
    Member(String),
    Everyone,
    Channel(String),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// The user's prompt to the manager.
    Prompt,
    Hire,
    Task,
    Direct,
    Broadcast,
    Channel,
    Report(Status),
    /// An agent's own answer: to the user from the manager, otherwise its
    /// result for the manager.
    Reply,
}

#[derive(Clone, Debug)]
pub struct Event {
    pub at: String,
    /// Sender session id; empty for the user.
    pub from: String,
    pub to: Target,
    pub kind: Kind,
    pub text: String,
}

#[derive(Clone, Debug)]
pub struct Team {
    pub manager: Member,
    pub title: String,
    pub members: Vec<Member>,
    pub events: Vec<Event>,
}

impl Team {
    pub fn member(&self, session_id: &str) -> Option<&Member> {
        std::iter::once(&self.manager)
            .chain(&self.members)
            .find(|m| m.session_id == session_id)
    }
}

struct Session {
    id: String,
    name: String,
    title: String,
    closed: bool,
    last_active: Option<String>,
    messages: Vec<Value>,
}

fn load_session(dir: &Path, id: &str) -> Option<Session> {
    let snapshot: Value =
        serde_json::from_str(&fs::read_to_string(dir.join(format!("{id}.json"))).ok()?).ok()?;
    let mut messages = snapshot
        .get("messages")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    if let Ok(journal) = fs::read_to_string(dir.join(format!("{id}.journal.jsonl"))) {
        for line in journal.lines() {
            if let Ok(entry) = serde_json::from_str::<Value>(line)
                && let Some(more) = entry.get("append_messages").and_then(Value::as_array)
            {
                messages.extend(more.iter().cloned());
            }
        }
    }
    let field = |key: &str| snapshot.get(key).and_then(Value::as_str).map(str::to_owned);
    Some(Session {
        id: id.to_owned(),
        name: field("short_name").unwrap_or_else(|| id.to_owned()),
        title: field("title").unwrap_or_default(),
        closed: field("status").as_deref() == Some("Closed"),
        last_active: field("last_active_at"),
        messages,
    })
}

fn session_ids(dir: &Path) -> Vec<String> {
    let mut ids: Vec<String> = fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|entry| {
            let name = entry.file_name().to_string_lossy().into_owned();
            let id = name.strip_suffix(".json")?;
            (id.starts_with("session_") && !id.ends_with(".journal")).then(|| id.to_owned())
        })
        .collect();
    ids.sort();
    ids
}

fn blocks(message: &Value) -> impl Iterator<Item = &Value> {
    message
        .get("content")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
}

fn text_of(message: &Value) -> String {
    blocks(message)
        .filter(|b| b.get("type").and_then(Value::as_str) == Some("text"))
        .filter_map(|b| b.get("text").and_then(Value::as_str))
        .collect::<Vec<_>>()
        .join("\n")
}

fn is_system_text(text: &str) -> bool {
    let text = text.trim_start();
    text.is_empty() || text.starts_with("<system-reminder>") || text.starts_with("<transcription")
}

/// Tool results by tool-use id.
fn tool_results(messages: &[Value]) -> HashMap<String, String> {
    let mut results = HashMap::new();
    for block in messages.iter().flat_map(blocks) {
        if block.get("type").and_then(Value::as_str) != Some("tool_result") {
            continue;
        }
        let Some(id) = block.get("tool_use_id").and_then(Value::as_str) else {
            continue;
        };
        let text = match block.get("content") {
            Some(Value::String(s)) => s.clone(),
            Some(Value::Array(parts)) => parts
                .iter()
                .filter_map(|p| p.get("text").and_then(Value::as_str))
                .collect::<Vec<_>>()
                .join("\n"),
            _ => String::new(),
        };
        results.insert(id.to_owned(), text);
    }
    results
}

fn spawned_ids(result: &str) -> impl Iterator<Item = &str> {
    result
        .split(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
        .filter(|word| word.starts_with("session_"))
}

/// Teams in the sessions folder, the most recently active first.
pub fn teams(sessions_dir: &Path) -> Vec<Team> {
    let ids = session_ids(sessions_dir);
    let sessions: BTreeMap<String, Session> = ids
        .iter()
        .filter_map(|id| load_session(sessions_dir, id).map(|s| (id.clone(), s)))
        .collect();
    let mut teams: Vec<Team> = sessions
        .values()
        .filter_map(|session| team_for(session, &sessions))
        .collect();
    teams.sort_by(|a, b| b.manager.last_active.cmp(&a.manager.last_active));
    teams
}

pub fn default_sessions_dir() -> Option<PathBuf> {
    Some(crate::platform_dirs::jcode_home()?.join("sessions"))
}

fn team_for(manager: &Session, sessions: &BTreeMap<String, Session>) -> Option<Team> {
    let results = tool_results(&manager.messages);
    // Resolve `to_session` given as a friendly name or an id.
    let mut members: Vec<Member> = Vec::new();
    let mut events = Vec::new();
    let mut hires = Vec::new();

    for message in &manager.messages {
        for block in blocks(message) {
            if block.get("type").and_then(Value::as_str) != Some("tool_use")
                || block.get("name").and_then(Value::as_str) != Some("swarm")
            {
                continue;
            }
            let input = block.get("input").cloned().unwrap_or(Value::Null);
            if input.get("action").and_then(Value::as_str) != Some("spawn") {
                continue;
            }
            let result = block
                .get("id")
                .and_then(Value::as_str)
                .and_then(|id| results.get(id))
                .map(String::as_str)
                .unwrap_or_default();
            for id in spawned_ids(result) {
                hires.push((id.to_owned(), input.clone(), timestamp(message)));
            }
        }
    }
    if hires.is_empty() {
        return None;
    }

    for (id, input, at) in &hires {
        let session = sessions.get(id);
        let role = input.get("label").and_then(Value::as_str).map(str::to_owned);
        let task = input.get("prompt").and_then(Value::as_str).map(str::to_owned);
        events.push(Event {
            at: at.clone(),
            from: manager.id.clone(),
            to: Target::Member(id.clone()),
            kind: Kind::Hire,
            text: match (&role, &task) {
                (Some(role), Some(task)) => format!("{role}\n{task}"),
                (Some(role), None) => role.clone(),
                (None, Some(task)) => task.clone(),
                (None, None) => String::new(),
            },
        });
        members.push(Member {
            session_id: id.clone(),
            name: session.map_or_else(|| short_name(id), |s| s.name.clone()),
            role,
            task,
            status: if session.is_some_and(|s| s.closed) {
                Status::Offline
            } else {
                Status::Ready
            },
            last_active: session.and_then(|s| s.last_active.clone()),
        });
    }

    let by_name: HashMap<String, String> = std::iter::once((manager.name.clone(), manager.id.clone()))
        .chain(members.iter().map(|m| (m.name.clone(), m.session_id.clone())))
        .collect();
    let resolve = |to: &str| -> String {
        by_name.get(to).cloned().unwrap_or_else(|| to.to_owned())
    };

    let mut statuses: HashMap<String, Status> = HashMap::new();
    let team_sessions: Vec<&Session> = std::iter::once(manager)
        .chain(members.iter().filter_map(|m| sessions.get(&m.session_id)))
        .collect();
    for session in &team_sessions {
        let is_manager = session.id == manager.id;
        for message in &session.messages {
            let at = timestamp(message);
            let role = message.get("role").and_then(Value::as_str).unwrap_or_default();
            if role == "user" && is_manager {
                let text = text_of(message);
                if !is_system_text(&text) {
                    events.push(Event {
                        at: at.clone(),
                        from: String::new(),
                        to: Target::Member(manager.id.clone()),
                        kind: Kind::Prompt,
                        text,
                    });
                }
            }
            if role != "assistant" {
                continue;
            }
            let text = text_of(message);
            if !is_system_text(&text) {
                events.push(Event {
                    at: at.clone(),
                    from: session.id.clone(),
                    to: if is_manager { Target::You } else { Target::Member(manager.id.clone()) },
                    kind: Kind::Reply,
                    text,
                });
            }
            for block in blocks(message) {
                if block.get("type").and_then(Value::as_str) != Some("tool_use")
                    || block.get("name").and_then(Value::as_str) != Some("swarm")
                {
                    continue;
                }
                let input = block.get("input").cloned().unwrap_or(Value::Null);
                let field = |k: &str| input.get(k).and_then(Value::as_str).map(str::to_owned);
                let body = field("message").unwrap_or_default();
                let target = field("to_session").or_else(|| field("target_session"));
                let event = match field("action").as_deref() {
                    Some("dm") => target.map(|to| (Target::Member(resolve(&to)), Kind::Direct, body)),
                    Some("message") => Some(match (target, field("channel")) {
                        (Some(to), _) => (Target::Member(resolve(&to)), Kind::Direct, body),
                        (None, Some(channel)) => (Target::Channel(channel), Kind::Channel, body),
                        (None, None) => (Target::Everyone, Kind::Broadcast, body),
                    }),
                    Some("broadcast") => Some((Target::Everyone, Kind::Broadcast, body)),
                    Some("channel") => {
                        Some((Target::Channel(field("channel").unwrap_or_default()), Kind::Channel, body))
                    }
                    Some("assign_task" | "assign_next" | "start_task" | "reassign") => {
                        let text = field("message")
                            .or_else(|| field("prompt"))
                            .or_else(|| field("task_id"))
                            .unwrap_or_default();
                        target.map(|to| (Target::Member(resolve(&to)), Kind::Task, text))
                    }
                    Some("report") => {
                        let status = field("status")
                            .and_then(|s| Status::from_report(&s))
                            .unwrap_or(Status::Ready);
                        statuses.insert(session.id.clone(), status);
                        let text = field("tldr").filter(|_| body.is_empty()).unwrap_or(body);
                        Some((Target::Member(manager.id.clone()), Kind::Report(status), text))
                    }
                    _ => None,
                };
                if let Some((to, kind, text)) = event {
                    events.push(Event {
                        at: at.clone(),
                        from: session.id.clone(),
                        to,
                        kind,
                        text,
                    });
                }
            }
        }
    }

    for member in &mut members {
        if member.status == Status::Offline {
            continue;
        }
        if let Some(status) = statuses.get(&member.session_id) {
            member.status = *status;
        } else if let Some(session) = sessions.get(&member.session_id) {
            // No report yet: busy while its last word is not an answer.
            let last = session.messages.last();
            let answered = last.is_some_and(|m| {
                m.get("role").and_then(Value::as_str) == Some("assistant")
                    && !text_of(m).trim().is_empty()
            });
            member.status = if answered { Status::Done } else { Status::Working };
        }
    }

    events.sort_by(|a, b| a.at.cmp(&b.at));
    // An agent's closing answer usually repeats the report it just sent.
    let mut last_report: HashMap<String, String> = HashMap::new();
    events.retain(|event| match event.kind {
        Kind::Report(_) => {
            last_report.insert(event.from.clone(), event.text.trim().to_owned());
            true
        }
        Kind::Reply => last_report.get(&event.from).is_none_or(|text| text != event.text.trim()),
        _ => true,
    });
    Some(Team {
        manager: Member {
            session_id: manager.id.clone(),
            name: manager.name.clone(),
            role: None,
            task: None,
            status: if manager.closed { Status::Offline } else { Status::Ready },
            last_active: manager.last_active.clone(),
        },
        title: manager.title.clone(),
        members,
        events,
    })
}

fn timestamp(message: &Value) -> String {
    message
        .get("timestamp")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned()
}

/// First line of a prompt sent with `/team`, as the team page shows it.
pub const GOAL_MARKER: &str = "👥 Team goal:";

/// What `/team <goal>` sends: the chat becomes the manager of a small team
/// that it hires with the `swarm` tool, matching installed skills to roles.
pub fn manager_prompt(goal: &str) -> String {
    format!(
        "{marker} {goal}

You are the manager of an agent team for this goal. Work like a small professional team:
1. Plan. Split the goal into the roles it really needs (usually 2 to 4, such as researcher, designer, developer, tester or writer). A small goal may need only one agent; never hire for show.
2. Skills. Call skill_manage with action=list. For each role pick the installed skills that fit it.
3. Hire. For each role call swarm with action=spawn, spawn_mode=\"headless\", a short role label in the user's language, and a prompt that gives the agent its task, what done looks like, the skills to load first with skill_manage action=load, and that it must finish with swarm action=report (status completed or blocked) and a short summary.
4. Coordinate. Pass results between agents with swarm action=dm, short and specific. Answer their questions and unblock them.
5. Finish. When everyone has reported, check the work and give the user one clear final answer in the user's language, saying what each agent did.
Keep the team small and its messages brief.",
        marker = GOAL_MARKER,
    )
}

/// The goal a team works on: the `/team` goal when it has one, otherwise the
/// first thing the user asked the manager, otherwise the session title.
pub fn goal(team: &Team) -> String {
    let prompt = team.events.iter().find(|e| e.kind == Kind::Prompt).map(|e| e.text.trim());
    match prompt {
        Some(text) => text
            .strip_prefix(GOAL_MARKER)
            .unwrap_or(text)
            .lines()
            .next()
            .unwrap_or_default()
            .trim()
            .to_owned(),
        None => team.title.clone(),
    }
}

/// `session_fox_1793…_ab12` → `fox`.
fn short_name(id: &str) -> String {
    id.strip_prefix("session_")
        .and_then(|rest| rest.split('_').next())
        .unwrap_or(id)
        .to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn write_session(dir: &Path, id: &str, name: &str, messages: Value, journal: &[Value]) {
        fs::create_dir_all(dir).unwrap();
        fs::write(
            dir.join(format!("{id}.json")),
            json!({
                "id": id, "short_name": name, "title": "Build the site", "status": "Active",
                "last_active_at": "2026-11-04T10:00:00Z", "messages": messages,
            })
            .to_string(),
        )
        .unwrap();
        if !journal.is_empty() {
            let lines: Vec<String> = journal
                .iter()
                .map(|m| json!({"meta": {}, "append_messages": [m]}).to_string())
                .collect();
            fs::write(dir.join(format!("{id}.journal.jsonl")), lines.join("\n")).unwrap();
        }
    }

    fn msg(role: &str, at: &str, content: Value) -> Value {
        json!({"role": role, "timestamp": at, "content": content})
    }

    fn swarm(id: &str, input: Value) -> Value {
        json!({"type": "tool_use", "id": id, "name": "swarm", "input": input})
    }

    fn result(id: &str, text: &str) -> Value {
        json!({"type": "tool_result", "tool_use_id": id, "content": text})
    }

    #[test]
    fn builds_a_team_with_roles_and_conversation() {
        let dir = tempfile::tempdir().unwrap();
        let sessions = dir.path();
        let boss = "session_otter_1_aa";
        let fox = "session_fox_2_bb";
        write_session(
            sessions,
            boss,
            "otter",
            json!([
                msg("user", "2026-11-04T09:00:00Z", json!([{"type": "text", "text": "<system-reminder>ctx</system-reminder>"}])),
                msg("user", "2026-11-04T09:00:01Z", json!([{"type": "text", "text": "یک سایت بساز"}])),
                msg("assistant", "2026-11-04T09:00:02Z", json!([swarm("c1", json!({"action": "spawn", "label": "designer", "prompt": "Design the page"}))])),
                msg("user", "2026-11-04T09:00:03Z", json!([result("c1", "Spawned new agent: session_fox_2_bb")])),
                msg("assistant", "2026-11-04T09:00:04Z", json!([swarm("c2", json!({"action": "dm", "to_session": "fox", "message": "Use purple"}))])),
            ]),
            &[],
        );
        write_session(
            sessions,
            fox,
            "fox",
            json!([msg("user", "2026-11-04T09:00:03Z", json!([{"type": "text", "text": "Design the page"}]))]),
            &[msg("assistant", "2026-11-04T09:00:05Z", json!([swarm("r1", json!({"action": "report", "status": "blocked", "message": "Need the logo"}))]))],
        );
        // A session that never hired anyone is not a team.
        write_session(sessions, "session_wolf_3_cc", "wolf", json!([]), &[]);

        let teams = teams(sessions);
        assert_eq!(teams.len(), 1);
        let team = &teams[0];
        assert_eq!(team.manager.name, "otter");
        assert_eq!(team.members.len(), 1);
        let member = &team.members[0];
        assert_eq!(member.name, "fox");
        assert_eq!(member.role.as_deref(), Some("designer"));
        assert_eq!(member.status, Status::Blocked);

        let kinds: Vec<Kind> = team.events.iter().map(|e| e.kind).collect();
        assert_eq!(
            kinds,
            [Kind::Prompt, Kind::Hire, Kind::Direct, Kind::Report(Status::Blocked)]
        );
        assert_eq!(team.events[0].text, "یک سایت بساز");
        assert_eq!(team.events[2].to, Target::Member(fox.into()));
        assert_eq!(team.events[3].from, fox);
        assert_eq!(team.events[3].to, Target::Member(boss.into()));
    }

    #[test]
    fn team_goal_comes_from_the_team_command() {
        let prompt = manager_prompt("یک سایت بساز");
        assert!(prompt.starts_with("👥 Team goal: یک سایت بساز
"));
        assert!(prompt.contains("skill_manage"));
        let team = Team {
            manager: Member {
                session_id: "m".into(),
                name: "otter".into(),
                role: None,
                task: None,
                status: Status::Ready,
                last_active: None,
            },
            title: "Title".into(),
            members: Vec::new(),
            events: vec![Event {
                at: String::new(),
                from: String::new(),
                to: Target::Member("m".into()),
                kind: Kind::Prompt,
                text: prompt,
            }],
        };
        assert_eq!(goal(&team), "یک سایت بساز");
    }

    #[test]
    fn worker_without_report_is_working_until_it_answers() {
        let dir = tempfile::tempdir().unwrap();
        let sessions = dir.path();
        write_session(
            sessions,
            "session_otter_1_aa",
            "otter",
            json!([
                msg("assistant", "2026-11-04T09:00:02Z", json!([swarm("c1", json!({"action": "spawn", "label": "tester"}))])),
                msg("user", "2026-11-04T09:00:03Z", json!([result("c1", "Spawned new agent: session_cat_2_bb")])),
            ]),
            &[],
        );
        write_session(
            sessions,
            "session_cat_2_bb",
            "cat",
            json!([msg("user", "2026-11-04T09:00:03Z", json!([{"type": "text", "text": "go"}]))]),
            &[],
        );
        assert_eq!(teams(sessions)[0].members[0].status, Status::Working);

        write_session(
            sessions,
            "session_cat_2_bb",
            "cat",
            json!([msg("assistant", "2026-11-04T09:01:00Z", json!([{"type": "text", "text": "All tests pass."}]))]),
            &[],
        );
        let team = &teams(sessions)[0];
        assert_eq!(team.members[0].status, Status::Done);
        assert!(team.events.iter().any(|e| e.kind == Kind::Reply && e.text == "All tests pass."));
    }
}
