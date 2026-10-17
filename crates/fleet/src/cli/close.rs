//! `fleet close <target>`: end a session that is alone in its own tmux session — the "one
//! tmux session per job" layout `fleet spawn` creates — and remove that tmux session.
//!
//! - **Exact target only** (`id:<session_id>` / `pid:<n>`): closing never matches by fragment.
//! - **This process only.** The agent is its pid plus its start time (`proc_start`, the
//!   identity check behind exact targets). `--expect-pid` / `--expect-start` name the process
//!   the caller saw; a session now run by another one is `target_changed`. Before anything is
//!   typed, and again before the kill, the process must still be that one.
//! - **Alone or nothing.** The session's pane must be the one pane of the one window of its
//!   tmux session. Anything else (a window in a shared session, a pane beside others) is
//!   `host_shared`; an iTerm tab is `backend_unsupported`. Nothing is typed or killed.
//! - **Graceful first**, under the per-session lock `send` / `keys` / `answer` take (`busy`
//!   when another fleet command holds it): a busy or waiting session gets Escape and up to
//!   [`SETTLE`] to go idle; then, if the agent still owns its pane, the harness's own exit
//!   command ([`crate::core::harness::Harness::exit_command`]) is typed and the agent gets
//!   `--timeout` seconds to exit. Only an agent in the foreground of its pane's terminal
//!   owns it: one suspended (Ctrl-Z) or exited (on Escape too) gets nothing typed, Escape
//!   included, and goes straight to the kill.
//! - **Then the tmux session is killed** by its id ([`tmux::kill_if_alone`]), whether the
//!   agent exited (its shell would stay) or not — in one tmux command with the check that the
//!   pane is still alone in it, so nothing renamed, replaced or split in between is hit.
//!
//! `-n` (or `FLEET_DRY_RUN=1`) checks all of that (lock included) and reports the plan, typing
//! and killing nothing.

use std::time::{Duration, Instant};

use colored::Colorize;
use serde_json::json;

use crate::cli::commands::{host_label, json_failure};
use crate::core::backend::{self, Key};
use crate::core::discovery::{self, Backend, Session, TargetError};
use crate::core::hosts;
use crate::core::lock::{self, LockError};
use crate::core::tmux::{self, HostSession, Hosting, KillAlone};
use crate::error::{Error, Result};

/// How long a busy or waiting session gets to settle after Escape.
const SETTLE: Duration = Duration::from_secs(3);
/// How often the session is looked at while waiting on it.
const POLL: Duration = Duration::from_millis(200);

pub struct Opts {
    pub target: String,
    pub json: bool,
    /// Seconds the agent gets to exit after its exit command.
    pub timeout: u64,
    /// The process the caller means (`--expect-pid` + `--expect-start`, given together).
    pub expect: Option<Expect>,
}

/// `--expect-pid` / `--expect-start`: the agent process a caller saw (a `list --json` row's
/// `pid` and `proc_start`).
pub struct Expect {
    pub pid: i64,
    pub start: String,
}

/// What `close` did (or, with `-n`, would do).
struct Closed {
    session: Session,
    tmux_session: String,
    /// `"exit"`: the agent was gone before the kill (it exited on its own command, on
    /// Escape, or by itself); `"killed"`: it still ran when its tmux session was killed.
    /// Under `-n`, the plan: `"exit"` when the agent owns its pane and the harness has an
    /// exit command (or it has exited already).
    how: &'static str,
}

/// The agent process: its pid and start time (RFC 3339), together what tells it from a
/// later process with the same pid.
struct Agent {
    pid: i64,
    start: String,
}

impl Agent {
    /// Still running, as this very process? A pid now another process's (the agent exited
    /// and its pid was reused) is not.
    fn alive(&self) -> bool {
        tmux::process_alive(self.pid)
            && discovery::process_start(self.pid)
                .is_some_and(|now| discovery::same_start(&now, &self.start))
    }
}

/// Where the agent stands with its pane, by [`owns`].
#[derive(Clone, Copy, PartialEq, Eq)]
enum Standing {
    /// Running in the foreground of the pane's terminal: what is typed there reaches it.
    Owns,
    /// Still on the pane's terminal but not in its foreground (suspended with Ctrl-Z, or
    /// put in the background): what is typed would reach the shell or job in front of it.
    Behind,
    /// Exited (its pid gone, or another process's now).
    Exited,
}

pub fn run(o: Opts) -> Result<()> {
    let dry_run = hosts::dry_run();
    let c = match close(&o, dry_run) {
        Ok(c) => c,
        Err(e) if o.json => return Err(json_failure(e)),
        Err(e) => return Err(e),
    };
    let s = &c.session;
    if o.json {
        let v = json!({
            "ok": true,
            "host": host_label(),
            "session_id": s.session_id,
            "pid": s.pid,
            "harness": s.harness.id(),
            "how": c.how,
            "tmux_session": c.tmux_session,
            "dry_run": dry_run,
        });
        println!("{}", serde_json::to_string_pretty(&v)?);
    } else {
        let what = match (dry_run, c.how) {
            (true, "exit") => "would type its exit command, then kill tmux session",
            (true, _) => "would kill tmux session",
            (false, "exit") => "exited; killed tmux session",
            (false, _) => "still running; killed tmux session",
        };
        println!(
            "closed {} ({}): {what} {}",
            s.headline().bold(),
            s.harness.id(),
            c.tmux_session
        );
    }
    Ok(())
}

fn close(o: &Opts, dry_run: bool) -> Result<Closed> {
    match discovery::exact_target(&o.target) {
        None => {
            return Err(Error::tagged(
                "usage",
                format!(
                    "\"{}\": close takes an exact target, id:<session_id> or pid:<n> — never a fragment",
                    o.target
                ),
            ));
        }
        Some(Err(e)) => return Err(e.into()),
        Some(Ok(_)) => {}
    }
    if let Some(e) = &o.expect
        && !discovery::same_start(&e.start, &e.start)
    {
        return Err(Error::tagged(
            "usage",
            format!(
                "--expect-start \"{}\": not a start time (RFC 3339, as list --json's proc_start)",
                e.start
            ),
        ));
    }
    if discovery::is_fixture() {
        return Err(Error::tagged(
            "backend_unsupported",
            "fixture mode: backends are inert (unset FLEET_FIXTURE)",
        ));
    }
    let s = discovery::resolve_target(&o.target)?;
    let Some(start) = start_of(&s) else {
        return Err(Error::tagged(
            "io",
            format!(
                "session {}: its process's start time cannot be read; nothing typed or killed",
                s.label()
            ),
        ));
    };
    let agent = Agent { pid: s.pid, start };
    if let Some(e) = &o.expect
        && (e.pid != agent.pid || !discovery::same_start(&e.start, &agent.start))
    {
        return Err(Error::tagged(
            "target_changed",
            format!(
                "{} is now pid {} started {}, not the expected pid {} started {}; nothing typed or killed",
                o.target, agent.pid, agent.start, e.pid, e.start
            ),
        ));
    }
    let pane = match (s.backend, s.handle.as_deref()) {
        (Backend::Tmux, Some(pane)) => pane.to_string(),
        _ => {
            return Err(Error::tagged(
                "backend_unsupported",
                format!(
                    "session {} runs in {}, not a tmux pane: close ends only a session alone in its own tmux session",
                    s.label(),
                    s.backend.label()
                ),
            ));
        }
    };
    let host = alone(tmux::hosting(&pane)?, &pane, "nothing typed or killed")?;
    let _lock = lock::try_lock(&s).map_err(|e| {
        let kind = match e {
            LockError::Busy => "busy",
            LockError::Io(_) => "io",
        };
        Error::tagged(kind, format!("{e}; nothing typed or killed"))
    })?;
    let exit = s.harness.exit_command();
    // Nothing is typed unless the agent owns its pane: one suspended or in the background
    // would leave Escape and its exit command to the shell in front of it, and one that
    // exited (on Escape too) to its shell, or whatever runs there now.
    let mut typed = "nothing typed";
    let mut standing = owns(&agent, &pane, typed)?;
    if dry_run {
        return Ok(Closed {
            how: match (standing, exit) {
                (Standing::Exited, _) | (Standing::Owns, Some(_)) => "exit",
                _ => "killed",
            },
            session: s,
            tmux_session: host.name,
        });
    }

    if standing == Standing::Owns && matches!(s.status.as_str(), "busy" | "waiting") {
        backend::keys(&s, &[Key::Escape])
            .map_err(|e| Error::tagged("io", format!("{e}; nothing typed or killed")))?;
        typed = "Escape was typed";
        settle(&o.target);
        standing = owns(&agent, &pane, typed)?;
    }
    let mut exited = match (standing, exit) {
        (Standing::Owns, Some(cmd)) => {
            backend::send(&s, cmd).map_err(|e| e.tag("io"))?;
            typed = "its exit command was typed";
            wait_exit(&agent, Duration::from_secs(o.timeout))
        }
        (Standing::Exited, _) => true,
        _ => false,
    };

    // Right before the kill: the session is still this process's (or gone with it).
    let untouched = format!("{typed}, nothing killed");
    still_ours(&o.target, &agent, &untouched)?;
    // It may have exited since the wait gave up on it.
    exited = exited || !agent.alive();
    let tmux_session = match tmux::kill_if_alone(&host.id, &pane)? {
        KillAlone::Killed(name) => name,
        KillAlone::Refused {
            windows,
            panes,
            linked,
            ours,
        } => {
            return Err(Error::tagged(
                "host_shared",
                if ours {
                    format!(
                        "tmux session {} now holds {windows} window(s), {panes} pane(s){}: close ends only a session alone in its own tmux session; {untouched}",
                        host.id,
                        if linked {
                            ", its window linked elsewhere"
                        } else {
                            ""
                        }
                    )
                } else {
                    format!(
                        "tmux session {} no longer holds the agent's pane {pane} alone; {untouched}",
                        host.id
                    )
                },
            ));
        }
        // The agent took its pane, and so its tmux session, with it — unless it still runs
        // somewhere else.
        KillAlone::Gone if agent.alive() => {
            return Err(Error::tagged(
                "target_changed",
                format!(
                    "tmux session {} is gone but pid {} still runs (moved elsewhere); {untouched}",
                    host.id, agent.pid
                ),
            ));
        }
        KillAlone::Gone => {
            exited = true;
            host.name
        }
    };
    Ok(Closed {
        how: if exited { "exit" } else { "killed" },
        session: s,
        tmux_session,
    })
}

/// The tmux session pane `pane` is alone in, by `hosting` (its [`tmux::hosting`]), or why
/// not: `host_shared`, or `target_not_found` when the pane is gone. `untouched` says what
/// was left undone.
fn alone(hosting: Hosting, pane: &str, untouched: &str) -> Result<HostSession> {
    match hosting {
        Hosting::Alone(host) => Ok(host),
        Hosting::Shared {
            session,
            windows,
            panes,
            linked,
        } => Err(Error::tagged(
            "host_shared",
            format!(
                "tmux session {session} holds {windows} window(s), {panes} pane(s){}: close ends only a session alone in its own tmux session; {untouched}",
                if linked {
                    ", its window linked elsewhere"
                } else {
                    ""
                }
            ),
        )),
        Hosting::Gone => Err(Error::tagged(
            "target_not_found",
            format!("pane {pane} is gone; {untouched}"),
        )),
    }
}

/// Does the agent still own pane `pane` — the same process (pid and start time), in the
/// foreground of that pane's terminal? [`Standing::Exited`] when it has exited (its pid
/// another process's now included: [`still_ours`] tells whether the target moved with it);
/// `target_changed` when it runs anywhere but that pane. `typed` says what was typed so far.
fn owns(agent: &Agent, pane: &str, typed: &str) -> Result<Standing> {
    if !agent.alive() {
        return Ok(Standing::Exited);
    }
    let pane_tty = tmux::pane_tty(pane)?;
    let tty = discovery::process_tty(agent.pid);
    let on = |t: &str| t.strip_prefix("/dev/").unwrap_or(t).to_string();
    match (pane_tty, tty) {
        (Some(p), Some(t)) if on(&p) == on(&t) => Ok(if discovery::process_foreground(agent.pid) {
            Standing::Owns
        } else if agent.alive() {
            Standing::Behind
        } else {
            Standing::Exited
        }),
        // It exited in between the two reads.
        _ if !agent.alive() => Ok(Standing::Exited),
        _ => Err(Error::tagged(
            "target_changed",
            format!(
                "pid {} no longer runs in pane {pane}; {typed}, nothing killed",
                agent.pid
            ),
        )),
    }
}

/// Right before the kill: `target` must name this agent still, or nothing live (it exited;
/// its pid may be another process's by now) — else `target_changed`.
fn still_ours(target: &str, agent: &Agent, untouched: &str) -> Result<()> {
    let changed = |now: String| {
        Err(Error::tagged(
            "target_changed",
            format!("{target} {now}; {untouched}"),
        ))
    };
    match discovery::resolve_target(target) {
        Ok(s)
            if s.pid == agent.pid
                && start_of(&s).is_some_and(|st| discovery::same_start(&st, &agent.start)) =>
        {
            Ok(())
        }
        Ok(s) => changed(format!(
            "is now pid {} started {}",
            s.pid,
            start_of(&s).as_deref().unwrap_or("at an unknown time")
        )),
        Err(TargetError::NotFound(_)) => Ok(()),
        Err(e) => changed(format!("no longer resolves to it: {e}")),
    }
}

/// The start time of session `s`'s process: as its lookup read it, else (a Codex / pi
/// session, which an exact lookup leaves without one) read now.
fn start_of(s: &Session) -> Option<String> {
    s.proc_start
        .clone()
        .or_else(|| discovery::process_start(s.pid))
}

/// Wait (≤ [`SETTLE`]) for the session to stop being busy or waiting — or to be gone.
fn settle(target: &str) {
    let deadline = Instant::now() + SETTLE;
    while Instant::now() < deadline {
        std::thread::sleep(POLL);
        match discovery::resolve_target(target) {
            Ok(s) if matches!(s.status.as_str(), "busy" | "waiting") => {}
            _ => return,
        }
    }
}

/// Wait (≤ `timeout`) for the agent to exit; whether it did.
fn wait_exit(agent: &Agent, timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    loop {
        if !agent.alive() {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(POLL);
    }
}

#[cfg(test)]
mod tests {
    use crate::core::harness::Harness;

    #[test]
    fn every_harness_has_a_plan() {
        assert_eq!(Harness::Claude.exit_command(), Some("/exit"));
        assert_eq!(Harness::Codex.exit_command(), Some("/quit"));
        assert_eq!(Harness::Pi.exit_command(), None);
    }
}
