//! Agent teams shared by the sidebar and the Team page. Reading the saved
//! sessions is cheap but not free, so both read one copy refreshed at most
//! every couple of seconds.
use jcode_desktop_model::team::{self, Team};
use std::cell::RefCell;
use std::time::{Duration, Instant};

pub(crate) const REFRESH: Duration = Duration::from_secs(2);

struct State {
    teams: Vec<Team>,
    selected: Option<String>,
    loaded_at: Option<Instant>,
}

thread_local! {
    static STATE: RefCell<State> = const {
        RefCell::new(State { teams: Vec::new(), selected: None, loaded_at: None })
    };
}

/// Teams, the most recently active first.
pub(crate) fn teams() -> Vec<Team> {
    STATE.with(|state| {
        let mut state = state.borrow_mut();
        if state.loaded_at.is_none_or(|at| at.elapsed() >= REFRESH) {
            state.teams = team::default_sessions_dir()
                .map(|dir| team::teams(&dir))
                .unwrap_or_default();
            state.loaded_at = Some(Instant::now());
        }
        state.teams.clone()
    })
}

/// The manager session id of the team on screen, if one was picked.
pub(crate) fn selected() -> Option<String> {
    STATE.with(|state| state.borrow().selected.clone())
}

pub(crate) fn select(manager_session_id: &str) {
    STATE.with(|state| state.borrow_mut().selected = Some(manager_session_id.to_owned()));
}
