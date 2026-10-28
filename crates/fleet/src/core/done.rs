//! `fleet done`: a session says its work is finished and nothing waits on the user. Nothing is
//! closed: `fleet list` stamps the mark on the row (`done: { at, note }`), the views show it (a
//! Done chip in the list, a Done card at the end of the conversation) and the user closes it.
//! The agent runs it from inside its own pane (`$TMUX_PANE`), right before its final message.
//!
//! Marks live in `$FLEET_DONE_STATE`, else `${XDG_STATE_HOME:-~/.local/state}/fleet/done.json`:
//! `{ "marks": [{ sessionId, pane, tmuxSession, at, note }] }`. A mark stops counting when the
//! user prompts the session after it (the conversation went on: the agent runs `fleet done`
//! again when it is finished again); `fleet bots tick` drops those and the ones whose session
//! is gone.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::core::discovery::Session;
use crate::error::{Error, Result};

/// What a session is told when it should report itself done once finished (bots, `spawn
/// --ask-done`, `fleet ask-done`).
pub const INSTRUCTION: &str = "When your work here is finished and nothing waits on the user \
(no question, review or decision for them), run `fleet done --note \"<one line: what you did>\"`, \
then end with a short final message: what you did, and what the user may want to check. Fleet \
then shows the session as done, and the user closes it. If anything needs the user, do not run \
it: say what you need and stop.";

/// `prompt` with [`INSTRUCTION`] below it (once).
pub fn with_instruction(prompt: &str) -> String {
    if prompt.contains(INSTRUCTION) {
        return prompt.to_string();
    }
    if prompt.trim().is_empty() {
        return INSTRUCTION.to_string();
    }
    format!("{}\n\n{INSTRUCTION}", prompt.trim_end())
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", default)]
pub struct Mark {
    pub session_id: String,
    /// The tmux pane id (`%12`) it ran in.
    pub pane: String,
    pub tmux_session: String,
    /// When it was marked done (ms epoch).
    pub at: i64,
    pub note: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct State {
    pub marks: Vec<Mark>,
}

pub fn state_path() -> PathBuf {
    if let Some(p) = std::env::var_os("FLEET_DONE_STATE").filter(|p| !p.is_empty()) {
        return PathBuf::from(crate::core::tools::expand_tilde(&p.to_string_lossy()));
    }
    let base = std::env::var_os("XDG_STATE_HOME")
        .filter(|p| !p.is_empty())
        .map(|p| PathBuf::from(crate::core::tools::expand_tilde(&p.to_string_lossy())))
        .unwrap_or_else(|| {
            dirs::home_dir()
                .unwrap_or_default()
                .join(".local")
                .join("state")
        });
    base.join("fleet").join("done.json")
}

pub fn load() -> State {
    std::fs::read_to_string(state_path())
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

pub fn save(st: &State) -> Result<()> {
    let p = state_path();
    if let Some(d) = p.parent() {
        std::fs::create_dir_all(d)?;
    }
    let tmp = p.with_extension("json.tmp");
    std::fs::write(&tmp, serde_json::to_string_pretty(st)?)?;
    std::fs::rename(&tmp, &p)?;
    Ok(())
}

/// The session running in `pane`, from a discovery pass.
pub fn session_in<'a>(rows: &'a [Session], pane: &str) -> Option<&'a Session> {
    rows.iter()
        .find(|r| r.handle.as_deref() == Some(pane) && r.tmux_session.is_some())
}

/// Mark (or, `cancel`, unmark) the session in `pane`. → the mark.
pub fn mark(rows: &[Session], pane: &str, note: Option<String>, cancel: bool) -> Result<Mark> {
    let s = session_in(rows, pane)
        .ok_or_else(|| Error::Other(format!("no agent session found in tmux pane {pane}")))?;
    let sid = s
        .session_id
        .clone()
        .filter(|i| !i.is_empty())
        .ok_or_else(|| Error::Other("this session has no session id yet".into()))?;
    let m = Mark {
        session_id: sid,
        pane: pane.into(),
        tmux_session: s.tmux_session.clone().unwrap_or_default(),
        at: chrono::Utc::now().timestamp_millis(),
        note: note.filter(|n| !n.trim().is_empty()),
    };
    let mut st = load();
    st.marks.retain(|x| x.session_id != m.session_id);
    if !cancel {
        st.marks.push(m.clone());
    }
    save(&st)?;
    Ok(m)
}

/// The mark the user has not answered yet, as `list` shows it.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct DoneRef {
    /// When it was marked done (ms epoch).
    pub at: i64,
    pub note: Option<String>,
}

/// Pure: `m` still stands for session `s` (`None` = gone): not prompted since.
pub fn stands(m: &Mark, s: Option<&Session>) -> bool {
    s.is_some_and(|s| s.prompted_at.is_none_or(|p| p <= m.at))
}

/// Put `done` on every row whose mark still stands. Read-only.
pub fn stamp_done(rows: &mut [Session]) {
    let st = load();
    if st.marks.is_empty() {
        return;
    }
    for r in rows.iter_mut() {
        let Some(id) = r.session_id.as_deref() else {
            continue;
        };
        r.done = st
            .marks
            .iter()
            .find(|m| m.session_id == id)
            .filter(|m| stands(m, Some(r)))
            .map(|m| DoneRef {
                at: m.at,
                note: m.note.clone(),
            });
    }
}

/// Drop the marks that no longer stand (session gone, or prompted again). → one log line each.
pub fn prune(rows: &[Session]) -> Result<Vec<String>> {
    let mut st = load();
    if st.marks.is_empty() {
        return Ok(Vec::new());
    }
    let mut log = Vec::new();
    let before = st.marks.len();
    st.marks.retain(|m| {
        let s = rows
            .iter()
            .find(|r| r.session_id.as_deref() == Some(m.session_id.as_str()));
        let keep = stands(m, s);
        if !keep {
            let why = if s.is_some() {
                "prompted again"
            } else {
                "gone"
            };
            log.push(format!("done: {} unmarked ({why})", m.tmux_session));
        }
        keep
    });
    if st.marks.len() != before {
        save(&st)?;
    }
    Ok(log)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(prompted: Option<i64>) -> Session {
        Session {
            session_id: Some("abc".into()),
            status: "idle".into(),
            prompted_at: prompted,
            ..Default::default()
        }
    }

    fn mark_at(at: i64) -> Mark {
        Mark {
            session_id: "abc".into(),
            at,
            ..Default::default()
        }
    }

    #[test]
    fn a_mark_stands_until_the_user_prompts_again() {
        let m = mark_at(1_000);
        assert!(!stands(&m, None));
        assert!(stands(&m, Some(&row(None))));
        assert!(stands(&m, Some(&row(Some(900)))));
        assert!(!stands(&m, Some(&row(Some(1_500)))));
    }

    #[test]
    fn with_instruction_appends_once() {
        let p = with_instruction("do x");
        assert!(p.starts_with("do x\n\n"));
        assert_eq!(with_instruction(&p), p);
        assert_eq!(with_instruction(" "), INSTRUCTION);
    }
}
