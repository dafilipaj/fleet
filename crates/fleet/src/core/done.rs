//! `fleet done`: a session says its work is finished and nothing waits on the user. Nothing is
//! closed: `fleet list` stamps the mark on the row (`done: { at, note }`), the views show it (a
//! Done chip in the list, a Done card at the end of the conversation) and the user closes it.
//! The agent runs it from inside its own pane, right before its final message: `$TMUX_PANE`, or
//! for Codex the session `$CODEX_THREAD_ID` names ([`own_pane`]).
//!
//! Marks live in `$FLEET_DONE_STATE`, else `${XDG_STATE_HOME:-~/.local/state}/fleet/done.json`:
//! `{ "marks": [{ sessionId, pane, tmuxSession, at, note }] }`. A mark stops counting when the
//! user prompts the session after it (the conversation went on: the agent runs `fleet done`
//! again when it is finished again), or when the session's main thread makes a tool call after
//! it (it went on working: the mark came too early, or from a subagent); `fleet bots tick` drops
//! those and the ones whose session is gone.
//!
//! **Only the session itself.** A subagent's shell, and a `claude -p` started from the session's
//! shell, inherit its `$TMUX_PANE`, so a mark from them would land on the session. For a Claude
//! Code session `fleet done` refuses when the nearest `claude` process above it isn't the
//! session's, or when its own call is in a subagent's transcript ([`find_caller`]).

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::core::discovery::Session;
use crate::core::harness::Harness;
use crate::error::{Error, Result};

/// What a session is told when it should report itself done once finished (bots, `spawn
/// --ask-done`, `fleet ask-done`).
pub const INSTRUCTION: &str = "When your work here is finished and nothing waits on the user \
(no question, review or decision for them), run `fleet done --note \"<one line: what you did>\"`, \
then end with a short final message: what you did, and what the user may want to check. Fleet \
then shows the session as done, and the user closes it. If anything needs the user, do not run \
it: say what you need and stop. A subagent never runs it: only the session itself, for its own \
work.";

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

/// The tmux pane of the agent session a command runs for. `$TMUX_PANE` does not do for Codex:
/// it runs commands through a shared app-server daemon, whose `$TMUX_PANE` is the pane that
/// started the daemon (another session's) or unset. Codex names the session in
/// `$CODEX_THREAD_ID` instead, its transcript id: the row's `session_id`. When that is set but
/// no Codex row in tmux has it, `None` — never the stale pane, which would be another session.
pub fn own_pane(
    rows: &[Session],
    codex_thread: Option<&str>,
    tmux_pane: Option<&str>,
) -> Option<String> {
    if let Some(t) = codex_thread.filter(|t| !t.is_empty()) {
        return rows
            .iter()
            .find(|r| {
                r.harness == Harness::Codex
                    && r.session_id.as_deref() == Some(t)
                    && r.tmux_session.is_some()
            })
            .and_then(|r| r.handle.clone());
    }
    tmux_pane.filter(|p| !p.is_empty()).map(String::from)
}

/// [`own_pane`] from this process's environment; discovers only when Codex asks.
pub fn own_pane_from_env(rows: Option<&[Session]>) -> Option<String> {
    let thread = std::env::var("CODEX_THREAD_ID").ok();
    let pane = std::env::var("TMUX_PANE").ok();
    if thread.as_deref().is_some_and(|t| !t.is_empty()) {
        let found;
        let rows = match rows {
            Some(r) => r,
            None => {
                found = crate::core::discovery::discover();
                &found
            }
        };
        return own_pane(rows, thread.as_deref(), pane.as_deref());
    }
    own_pane(&[], None, pane.as_deref())
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

/// Pure: `m` still stands for session `s` (`None` = gone): not prompted since, and no tool
/// call of its main thread since (the `fleet done` call itself is older than the mark).
pub fn stands(m: &Mark, s: Option<&Session>) -> bool {
    s.is_some_and(|s| {
        s.prompted_at.is_none_or(|p| p <= m.at) && s.tool_call_at.is_none_or(|t| t <= m.at)
    })
}

/// Who made a `fleet done` call that is still waiting for its result.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Caller {
    /// The session's main thread.
    Session,
    /// A subagent: its call is in its own transcript, or marked `isSidechain`.
    Subagent,
}

/// How much of a transcript's tail [`find_caller`] reads.
const MAIN_TAIL: u64 = 256 * 1024;
const SIDECAR_TAIL: u64 = 64 * 1024;
/// A sidecar not written for this long holds no call running now.
const SIDECAR_FRESH: std::time::Duration = std::time::Duration::from_secs(120);

/// Pure: `command` runs `fleet done` (not just mentions it inside a word).
fn runs_fleet_done(command: &str) -> bool {
    command.match_indices("fleet done").any(|(i, _)| {
        let before = command[..i].chars().next_back();
        let after = command[i + "fleet done".len()..].chars().next();
        // `/` for a path to the binary (`~/.cargo/bin/fleet done`).
        before.is_none_or(|c| c.is_whitespace() || ";&|(`$/".contains(c))
            && after.is_none_or(|c| c.is_whitespace() || ";&|)`".contains(c))
    })
}

/// Pure: the newest `fleet done` call in `text`, a transcript's JSONL tail, with no result
/// yet. Lines that don't parse (a tail's cut first line) are skipped.
pub fn pending_call(text: &str) -> Option<Caller> {
    let mut pending: Vec<(String, Caller)> = Vec::new();
    for line in text.lines() {
        if !line.contains("fleet done") && !line.contains("tool_result") {
            continue;
        }
        let Ok(v) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        let Some(content) = v["message"]["content"].as_array() else {
            continue;
        };
        for c in content {
            match c["type"].as_str() {
                Some("tool_use") if v["type"] == "assistant" => {
                    let runs = c["input"]["command"].as_str().is_some_and(runs_fleet_done);
                    if let (true, Some(id)) = (runs, c["id"].as_str()) {
                        let who = if v["isSidechain"] == true {
                            Caller::Subagent
                        } else {
                            Caller::Session
                        };
                        pending.push((id.to_string(), who));
                    }
                }
                Some("tool_result") => {
                    if let Some(id) = c["tool_use_id"].as_str() {
                        pending.retain(|(p, _)| p != id);
                    }
                }
                _ => {}
            }
        }
    }
    pending.last().map(|(_, who)| *who)
}

/// The last `max` bytes of `path`, whole lines only.
fn tail_text(path: &std::path::Path, max: u64) -> Option<String> {
    use std::io::{Read, Seek, SeekFrom};
    let mut f = std::fs::File::open(path).ok()?;
    let size = f.metadata().ok()?.len();
    let start = size.saturating_sub(max);
    f.seek(SeekFrom::Start(start)).ok()?;
    let mut buf = Vec::new();
    f.read_to_end(&mut buf).ok()?;
    let text = String::from_utf8_lossy(&buf).into_owned();
    Some(if start > 0 {
        text.find('\n')
            .map_or(String::new(), |i| text[i + 1..].to_string())
    } else {
        text
    })
}

/// A subagent of the session whose transcript is `transcript` has a `fleet done` call running.
fn subagent_call(transcript: &std::path::Path) -> bool {
    let dir = transcript.with_extension("").join("subagents");
    let Ok(entries) = std::fs::read_dir(dir) else {
        return false;
    };
    let now = std::time::SystemTime::now();
    entries.flatten().any(|e| {
        let p = e.path();
        let sidecar = p.extension().is_some_and(|x| x == "jsonl")
            && p.file_name()
                .is_some_and(|n| n.to_string_lossy().starts_with("agent-"));
        let fresh = e
            .metadata()
            .and_then(|m| m.modified())
            .is_ok_and(|m| now.duration_since(m).unwrap_or_default() < SIDECAR_FRESH);
        sidecar && fresh && tail_text(&p, SIDECAR_TAIL).is_some_and(|t| pending_call(&t).is_some())
    })
}

/// Who runs this `fleet done`, from where its call is recorded. Claude Code writes a call to
/// the transcript about a second after it starts, so this looks again until `wait` is up; a
/// subagent's call is looked for first, as refusing is the safe side. `None` when the call
/// shows nowhere in time (the mark is then made, as before).
pub fn find_caller(transcript: &std::path::Path, wait: std::time::Duration) -> Option<Caller> {
    let deadline = std::time::Instant::now() + wait;
    loop {
        if subagent_call(transcript) {
            return Some(Caller::Subagent);
        }
        if let Some(c) = tail_text(transcript, MAIN_TAIL).and_then(|t| pending_call(&t)) {
            return Some(c);
        }
        if std::time::Instant::now() >= deadline {
            return None;
        }
        std::thread::sleep(std::time::Duration::from_millis(150));
    }
}

/// The pid of the nearest `claude` process above this one: the Claude Code whose tool ran it.
pub fn claude_ancestor() -> Option<i64> {
    let mut pid = std::process::id() as i64;
    for _ in 0..12 {
        let out = std::process::Command::new("ps")
            .args(["-o", "ppid=", "-p", &pid.to_string()])
            .output()
            .ok()?;
        let ppid: i64 = String::from_utf8_lossy(&out.stdout).trim().parse().ok()?;
        if ppid <= 1 {
            return None;
        }
        let comm = std::process::Command::new("ps")
            .args(["-o", "comm=", "-p", &ppid.to_string()])
            .output()
            .ok()?;
        let comm = String::from_utf8_lossy(&comm.stdout).trim().to_string();
        if std::path::Path::new(&comm)
            .file_name()
            .is_some_and(|n| n == "claude")
        {
            return Some(ppid);
        }
        pid = ppid;
    }
    None
}

/// Pure: why `fleet done` must not mark `s`, given the nearest `claude` above it and where
/// its call was found; `None` when it may.
pub fn refusal(s: &Session, ancestor: Option<i64>, caller: Option<Caller>) -> Option<String> {
    if s.harness != Harness::Claude {
        return None;
    }
    if let Some(a) = ancestor.filter(|a| *a != s.pid) {
        return Some(format!(
            "fleet done: run by another Claude Code (pid {a}) than the session in this pane \
             (pid {}), such as a `claude -p` started from its shell; only the session itself \
             reports that its work is finished",
            s.pid
        ));
    }
    (caller == Some(Caller::Subagent)).then(|| {
        "fleet done: a subagent can't mark its session done; only the session itself reports \
         that its work is finished. Finish with your report instead."
            .to_string()
    })
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
            let why = match s {
                None => "gone",
                Some(s) if s.prompted_at.is_some_and(|p| p > m.at) => "prompted again",
                Some(_) => "kept working",
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
    fn a_mark_stops_standing_once_the_main_thread_calls_a_tool_after_it() {
        let m = mark_at(1_000);
        let worked = |t| Session {
            tool_call_at: Some(t),
            ..row(Some(900))
        };
        // The `fleet done` call itself is older than the mark.
        assert!(stands(&m, Some(&worked(990))));
        // A subagent's mark: the parent goes on with a tool call when its result comes back.
        assert!(!stands(&m, Some(&worked(1_200))));
    }

    fn call(id: &str, command: &str, sidechain: bool) -> String {
        serde_json::json!({
            "type": "assistant", "isSidechain": sidechain, "timestamp": "2026-11-05T10:00:00Z",
            "message": {"content": [{"type": "tool_use", "id": id, "name": "Bash",
                "input": {"command": command}}]}
        })
        .to_string()
    }

    fn result(id: &str) -> String {
        serde_json::json!({
            "type": "user",
            "message": {"content": [{"type": "tool_result", "tool_use_id": id, "content": "ok"}]}
        })
        .to_string()
    }

    #[test]
    fn the_running_fleet_done_call_says_who_made_it() {
        let done = r#"fleet done --note "fixed it""#;
        assert_eq!(pending_call(&call("a", done, false)), Some(Caller::Session));
        assert_eq!(pending_call(&call("a", done, true)), Some(Caller::Subagent));
        // Answered: an earlier call, not this one.
        assert_eq!(
            pending_call(&[call("a", done, false), result("a")].join("\n")),
            None
        );
        // Mentioning it is not running it; a cut first line is skipped.
        assert_eq!(
            pending_call(&call("a", "grep -r 'fleetdone' .", false)),
            None
        );
        assert_eq!(
            pending_call(&format!("{{\"cut\n{}", call("b", done, false))),
            Some(Caller::Session)
        );
        assert!(runs_fleet_done("cd x && fleet done --cancel"));
        assert!(runs_fleet_done("/Users/me/fleet/target/debug/fleet done --note x"));
        assert!(!runs_fleet_done("echo fleet doner"));
    }

    fn transcript_with_sidecar(
        main: &[String],
        sidecar: Option<&[String]>,
    ) -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let t = dir.path().join("abc.jsonl");
        std::fs::write(&t, main.join("\n") + "\n").unwrap();
        if let Some(lines) = sidecar {
            let sub = dir.path().join("abc").join("subagents");
            std::fs::create_dir_all(&sub).unwrap();
            std::fs::write(sub.join("agent-a1.jsonl"), lines.join("\n") + "\n").unwrap();
        }
        (dir, t)
    }

    #[test]
    fn a_subagents_fleet_done_is_found_in_its_own_transcript() {
        let done = "fleet done --note review finished";
        let parent = [
            call("p1", "ls", false),
            result("p1"),
            call("p2", "Agent: review", false),
        ];
        // The Subagent's call is in its sidecar, not in the session's transcript.
        let (_d, t) = transcript_with_sidecar(&parent, Some(&[call("s1", done, false)]));
        assert_eq!(
            find_caller(&t, std::time::Duration::ZERO),
            Some(Caller::Subagent)
        );
        // Its earlier, answered call doesn't count; the session's own call then does.
        let mut own = parent.to_vec();
        own.push(call("p3", done, false));
        let (_d, t) = transcript_with_sidecar(&own, Some(&[call("s1", done, false), result("s1")]));
        assert_eq!(
            find_caller(&t, std::time::Duration::ZERO),
            Some(Caller::Session)
        );
        // Nowhere in time: unknown, and the mark is made as before.
        let (_d, t) = transcript_with_sidecar(&parent, None);
        assert_eq!(find_caller(&t, std::time::Duration::ZERO), None);
    }

    #[test]
    fn only_the_session_itself_may_mark_a_claude_session_done() {
        let s = Session {
            pid: 42,
            ..row(None)
        };
        assert_eq!(refusal(&s, Some(42), Some(Caller::Session)), None);
        assert_eq!(refusal(&s, None, None), None);
        assert!(
            refusal(&s, Some(42), Some(Caller::Subagent))
                .unwrap()
                .contains("subagent")
        );
        // A `claude -p` from the session's shell: another Claude Code above it.
        assert!(refusal(&s, Some(77), None).unwrap().contains("claude -p"));
        // Codex finds its session by $CODEX_THREAD_ID, so none of this applies.
        let codex = Session {
            harness: Harness::Codex,
            ..s
        };
        assert_eq!(refusal(&codex, Some(77), Some(Caller::Subagent)), None);
    }

    #[test]
    fn a_mark_stands_until_the_user_prompts_again() {
        let m = mark_at(1_000);
        assert!(!stands(&m, None));
        assert!(stands(&m, Some(&row(None))));
        assert!(stands(&m, Some(&row(Some(900)))));
        assert!(!stands(&m, Some(&row(Some(1_500)))));
    }

    fn codex_row(id: &str, pane: &str) -> Session {
        Session {
            session_id: Some(id.into()),
            harness: Harness::Codex,
            handle: Some(pane.into()),
            tmux_session: Some("job".into()),
            ..Default::default()
        }
    }

    #[test]
    fn own_pane_is_tmux_pane_outside_codex() {
        assert_eq!(own_pane(&[], None, Some("%3")), Some("%3".into()));
        assert_eq!(own_pane(&[], None, None), None);
        assert_eq!(own_pane(&[], Some(""), Some("%3")), Some("%3".into()));
    }

    #[test]
    fn codex_finds_its_pane_by_thread_not_the_daemons_stale_pane() {
        let rows = [codex_row("t1", "%7"), codex_row("t2", "%9")];
        assert_eq!(own_pane(&rows, Some("t2"), Some("%7")), Some("%9".into()));
        // unknown thread: no guess, and never the stale pane (another session's)
        assert_eq!(own_pane(&rows, Some("t3"), Some("%7")), None);
        // a Claude row with the same id does not count
        let claude = Session {
            harness: Harness::Claude,
            ..codex_row("t3", "%5")
        };
        assert_eq!(own_pane(&[claude], Some("t3"), Some("%5")), None);
    }

    #[test]
    fn with_instruction_appends_once() {
        let p = with_instruction("do x");
        assert!(p.starts_with("do x\n\n"));
        assert_eq!(with_instruction(&p), p);
        assert_eq!(with_instruction(" "), INSTRUCTION);
    }
}
