//! `fleet done`: a session says its work is finished and nothing waits on the user, and is
//! closed once it has been quiet for [`QUIET_MS`]. The agent runs it from inside its own pane
//! (`$TMUX_PANE`); `fleet bots tick` (the `fleet.jobs` service) does the closing.
//!
//! Marks live in `$FLEET_DONE_STATE`, else `${XDG_STATE_HOME:-~/.local/state}/fleet/done.json`:
//! `{ "marks": [{ sessionId, pane, tmuxSession, at, note }] }`. A mark is dropped when its session
//! is gone, or when the user prompts it after the mark (the conversation went on: the agent runs
//! `fleet done` again when it is finished again). A session waiting for input, working, or
//! running background agents or shells is never closed.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::core::discovery::Session;
use crate::error::{Error, Result};

/// How long a done session must be quiet before it is closed.
pub const QUIET_MS: i64 = 3600 * 1000;

/// What a session is told when it should close itself once finished (bots, `spawn
/// --close-when-done`, `fleet close-when-done`).
pub const INSTRUCTION: &str = "When your work here is finished and nothing waits on the user \
(no question, review or decision for them), run `fleet done --note \"<one line: what you did>\"` \
as your last step: it closes this session after an hour of quiet. If anything needs the user, \
do not run it — say what you need and stop.";

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

/// What [`sweep`] decides for one mark.
#[derive(Debug, Clone, PartialEq)]
pub enum Verdict {
    /// Not yet: still busy, or not quiet long enough.
    Wait,
    /// Drop the mark (gone, or the user picked the conversation up again).
    Drop(&'static str),
    Close,
}

/// Pure: what to do with `m`, given its session (`None` = gone) at `now`.
pub fn judge(m: &Mark, s: Option<&Session>, now: i64) -> Verdict {
    let Some(s) = s else {
        return Verdict::Drop("gone");
    };
    if s.prompted_at.is_some_and(|p| p > m.at) {
        return Verdict::Drop("prompted again");
    }
    let busy = s.status != "idle" || s.background.as_ref().is_some_and(|b| !b.is_empty());
    let last = s.updated_at.unwrap_or(m.at).max(m.at);
    if busy || now - last < QUIET_MS {
        return Verdict::Wait;
    }
    Verdict::Close
}

/// Close the done sessions that are quiet; drop stale marks. → one log line per action.
pub fn sweep(rows: &[Session], now: i64) -> Result<Vec<String>> {
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
        match judge(m, s, now) {
            Verdict::Wait => true,
            Verdict::Drop(why) => {
                log.push(format!("done: {} unmarked ({why})", m.tmux_session));
                false
            }
            Verdict::Close => match close(s.and_then(|s| s.handle.as_deref()).unwrap_or(&m.pane)) {
                Ok(what) => {
                    log.push(format!("done: closed {what}"));
                    false
                }
                Err(e) => {
                    log.push(format!("done: could not close {}: {e}", m.tmux_session));
                    true
                }
            },
        }
    });
    if st.marks.len() != before {
        save(&st)?;
    }
    Ok(log)
}

/// Close the pane `pane`: its whole tmux session when it is the only pane there.
fn close(pane: &str) -> Result<String> {
    let session =
        crate::core::backend::tmux_query(pane, "#{session_name}").map_err(Error::Other)?;
    let (_, panes) = crate::core::backend::tmux_shape(pane).map_err(Error::Other)?;
    if panes <= 1 {
        crate::core::tmux::kill_session(&session)?;
        Ok(session)
    } else {
        let out = crate::core::tools::tmux_cmd()
            .args(["kill-pane", "-t", pane])
            .output()?;
        if !out.status.success() {
            return Err(Error::Other(
                String::from_utf8_lossy(&out.stderr).trim().to_string(),
            ));
        }
        Ok(format!("{session} (pane {pane})"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(status: &str, updated: i64, prompted: Option<i64>) -> Session {
        Session {
            session_id: Some("abc".into()),
            status: status.into(),
            updated_at: Some(updated),
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
    fn closes_only_when_idle_and_quiet() {
        let m = mark_at(1_000);
        let h = QUIET_MS;
        assert_eq!(judge(&m, None, 0), Verdict::Drop("gone"));
        assert_eq!(
            judge(&m, Some(&row("idle", 2_000, None)), 2_000 + h - 1),
            Verdict::Wait
        );
        assert_eq!(
            judge(&m, Some(&row("idle", 2_000, None)), 2_000 + h),
            Verdict::Close
        );
        assert_eq!(
            judge(&m, Some(&row("busy", 2_000, None)), 9 * h),
            Verdict::Wait
        );
        assert_eq!(
            judge(&m, Some(&row("waiting", 2_000, None)), 9 * h),
            Verdict::Wait
        );
        // Quiet counts from the mark when the session's last update is older.
        assert_eq!(
            judge(&m, Some(&row("idle", 0, None)), 1_000 + h),
            Verdict::Close
        );
    }

    #[test]
    fn a_prompt_after_the_mark_drops_it() {
        let m = mark_at(1_000);
        assert_eq!(
            judge(&m, Some(&row("idle", 0, Some(900))), 9 * QUIET_MS),
            Verdict::Close
        );
        assert_eq!(
            judge(&m, Some(&row("idle", 0, Some(1_500))), 9 * QUIET_MS),
            Verdict::Drop("prompted again")
        );
    }

    #[test]
    fn background_work_holds_it_open() {
        let mut s = row("idle", 0, None);
        s.background = Some(crate::core::background::Background {
            agents: Vec::new(),
            shells: 1,
        });
        assert_eq!(judge(&mark_at(0), Some(&s), 9 * QUIET_MS), Verdict::Wait);
    }
}
