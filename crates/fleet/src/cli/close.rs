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
//!   `--timeout` seconds to exit — unless its input box holds text (the prompt Escape put
//!   back, a draft) or a prompt waits there ([`box_clear`]): the command would go out with
//!   that text, or answer that prompt, so it goes straight to the kill. Only an agent in the
//!   foreground of its pane's terminal owns it ([`owns`], from one `ps`): one suspended
//!   (Ctrl-Z) or exited (on Escape too) gets nothing typed, Escape included, and goes
//!   straight to the kill — unless something else is in front of it: a process of its own
//!   (Claude Code's Ctrl-G editor) is `busy`, another job is `host_shared`, and nothing is
//!   typed or killed. Another job stopped on the pane's terminal (the kill would end it too)
//!   is `host_shared` as well. The same look is taken again right before the kill. `busy`
//!   and `host_shared` failures carry a `reason`
//!   ([`LOCKED`], [`EDITOR_OPEN`], [`SHARED_HOST`], [`JOB_IN_FRONT`], [`OTHER_JOBS`]).
//! - **Fails closed:** when who reads the pane's terminal cannot be told (`lsof` missing,
//!   failing, or slower than [`discovery::LSOF_DEADLINE`]) it is `io`, with nothing typed.
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
use crate::core::discovery::{self, Backend, Proc, Session, TargetError};
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
    /// This very process in `procs`, if it still runs: its pid, with its start time — a pid
    /// now another process's (the agent exited and its pid was reused) is not it.
    fn find<'a>(&self, procs: &'a [Proc]) -> Option<&'a Proc> {
        procs.iter().find(|p| {
            p.pid == self.pid
                && !p.zombie()
                && p.start
                    .as_deref()
                    .is_some_and(|s| discovery::same_start(s, &self.start))
        })
    }

    /// Still running, as this very process? Read from one `ps` of its pid alone, not of every
    /// process. `io` when `ps` cannot be run.
    fn alive(&self) -> Result<bool> {
        let procs = discovery::processes_of(&[self.pid])
            .map_err(|e| Error::tagged("io", format!("ps: {e}")))?;
        Ok(self.find(&procs).is_some())
    }
}

/// `busy` reasons: another fleet command holds the session's lock; a process of the agent's
/// own (its external editor) is in front of it.
const LOCKED: &str = "locked";
const EDITOR_OPEN: &str = "editor_open";
/// `host_shared` reasons: its tmux session holds more than its pane (or no longer holds it);
/// another job is in front of the agent; another job is stopped on its pane's terminal.
const SHARED_HOST: &str = "shared_host";
const JOB_IN_FRONT: &str = "job_in_front";
const OTHER_JOBS: &str = "other_jobs";

/// Where the agent stands with its pane, by [`owns`].
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Standing {
    /// Running in the foreground of the pane's terminal, nothing of its own in front of it:
    /// what is typed there reaches it.
    Owns,
    /// Still on the pane's terminal but not in its foreground (suspended with Ctrl-Z, or
    /// put in the background), with nothing in front of it but its own stopped group or the
    /// shell that started it, at its prompt: what is typed would reach that shell, and the
    /// kill hits nothing else.
    Behind,
    /// Exited (its pid gone, or another process's now).
    Exited,
}

/// What one [`discovery::processes`] snapshot says of the agent and its pane, before the
/// check on who reads the terminal ([`reading_tty`](discovery::reading_tty)).
#[derive(Debug, PartialEq, Eq)]
enum Look<'a> {
    Exited,
    /// Alive, but not on the pane's terminal (or the pane is gone).
    Elsewhere,
    /// In the foreground of the pane's terminal `tty`. `children` are its descendants in
    /// that foreground group, on that terminal: any of them that reads it (an editor it
    /// opened, as Claude Code's Ctrl-G does) gets what is typed instead. `other`: a stopped
    /// job of the pane's shell behind it ([`Look::Others`]), told only once no child reads.
    Front {
        tty: &'a str,
        children: Vec<&'a Proc>,
        other: Option<&'a Proc>,
    },
    Behind,
    /// Not in front: a process of its own is, in a group of its own (an editor) — `busy`.
    Covered(&'a Proc),
    /// Not in front: another job is (one the user ran after Ctrl-Z) — `host_shared`.
    Job(&'a Proc),
    /// Nothing but itself or its shell in front, but another job is stopped on the pane's
    /// terminal (`vim`, Ctrl-Z): the kill would end it too — `host_shared`.
    Others(&'a Proc),
}

/// [`Look`] of `agent` in `procs`, on a pane whose terminal is `pane_tty`.
fn look<'a>(procs: &'a [Proc], agent: &Agent, pane_tty: Option<&str>) -> Look<'a> {
    let Some(me) = agent.find(procs) else {
        return Look::Exited;
    };
    let dev = |t: &str| t.strip_prefix("/dev/").unwrap_or(t).to_string();
    let tty = match (pane_tty, me.tty.as_deref()) {
        (Some(p), Some(t)) if dev(p) == dev(t) => t,
        _ => return Look::Elsewhere,
    };
    let parent = |p: &Proc| procs.iter().find(|q| q.pid == p.ppid && p.ppid > 1);
    let descends = |p: &Proc| {
        let mut at = parent(p);
        for _ in 0..64 {
            match at {
                Some(q) if q.pid == me.pid => return true,
                Some(q) => at = parent(q),
                None => return false,
            }
        }
        false
    };
    // A job of the pane's shell stopped on its terminal (not the agent's own group, nor
    // anything it started). Running background jobs are left out: a shell's own helpers
    // (a prompt's `gitstatusd`) run that way.
    let other = procs.iter().find(|p| {
        p.tty.as_deref() == Some(tty)
            && p.stopped()
            && !p.zombie()
            && p.pgid != me.pgid
            && !descends(p)
    });
    if me.in_front() {
        let children = procs
            .iter()
            .filter(|p| {
                p.pid != me.pid
                    && p.pgid == me.tpgid
                    && p.tty.as_deref() == Some(tty)
                    && !p.zombie()
                    && descends(p)
            })
            .collect();
        return Look::Front {
            tty,
            children,
            other,
        };
    }
    let behind = || other.map_or(Look::Behind, Look::Others);
    let front: Vec<&Proc> = procs
        .iter()
        .filter(|p| p.pgid == me.tpgid && me.tpgid > 0 && !p.zombie())
        .collect();
    if me.tpgid == me.pgid || front.is_empty() {
        return behind();
    }
    if let Some(c) = front.iter().find(|p| descends(p)) {
        return Look::Covered(c);
    }
    // The shell that started it, at its prompt, is one of its ancestors.
    let mut ancestors = Vec::new();
    let mut at = parent(me);
    while let Some(q) = at.filter(|_| ancestors.len() < 64) {
        ancestors.push(q.pid);
        at = parent(q);
    }
    if front.iter().any(|p| ancestors.contains(&p.pid)) {
        return behind();
    }
    Look::Job(
        front
            .iter()
            .find(|p| p.pid == me.tpgid)
            .unwrap_or(&front[0]),
    )
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
        let msg = format!("{e}; nothing typed or killed");
        match e {
            LockError::Busy => Error::tagged_for("busy", LOCKED, msg),
            LockError::Io(_) => Error::tagged("io", msg),
        }
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
        (Standing::Owns, Some(_)) if !box_clear(&s) => false,
        (Standing::Owns, Some(cmd)) => {
            backend::send(&s, cmd).map_err(|e| e.tag("io"))?;
            typed = "its exit command was typed";
            wait_exit(&agent, Duration::from_secs(o.timeout))
        }
        (Standing::Exited, _) => true,
        _ => false,
    };

    // Right before the kill: the session is still this process's (or gone with it), and
    // still owns its pane — or stands behind it with nothing else in front — read at once.
    let untouched = format!("{typed}, nothing killed");
    still_ours(&o.target, &agent, &untouched)?;
    // It may have exited since the wait gave up on it.
    exited = owns(&agent, &pane, typed)? == Standing::Exited || exited;
    let tmux_session = match tmux::kill_if_alone(&host.id, &pane)? {
        KillAlone::Killed(name) => name,
        KillAlone::Refused {
            windows,
            panes,
            linked,
            ours,
        } => {
            return Err(Error::tagged_for(
                "host_shared",
                SHARED_HOST,
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
        KillAlone::Gone
            if agent
                .alive()
                .map_err(|e| Error::tagged("io", format!("{e}; {untouched}")))? =>
        {
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
        } => Err(Error::tagged_for(
            "host_shared",
            SHARED_HOST,
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
/// foreground of that pane's terminal, with nothing of its own reading it in front of it?
/// Its pid, start time, terminal and foreground state come from one `ps`
/// ([`discovery::processes`]). [`Standing::Exited`] when it has exited (its pid another
/// process's now included: [`still_ours`] tells whether the target moved with it). Refused,
/// with `typed` (what was typed so far) in the message: `target_changed` when it runs
/// anywhere but that pane; `busy` when a process of its own is in front of it (an editor);
/// `host_shared` when another job is, the agent suspended behind it.
fn owns(agent: &Agent, pane: &str, typed: &str) -> Result<Standing> {
    let untouched = format!("{typed}, nothing killed");
    let pane_tty =
        tmux::pane_tty(pane).map_err(|e| Error::tagged(e.kind(), format!("{e}; {untouched}")))?;
    let procs =
        discovery::processes().map_err(|e| Error::tagged("io", format!("ps: {e}; {untouched}")))?;
    let in_front = |p: &Proc, of: &str| {
        format!(
            "pid {} ({}) is in front of the agent in pane {pane}{of}; {untouched}",
            p.pid, p.command
        )
    };
    let others = |p: &Proc| {
        Err(Error::tagged_for(
            "host_shared",
            OTHER_JOBS,
            format!(
                "pid {} ({}) is a stopped job on the agent's terminal in pane {pane}: closing would kill that job too; {untouched}",
                p.pid, p.command
            ),
        ))
    };
    match look(&procs, agent, pane_tty.as_deref()) {
        Look::Exited => Ok(Standing::Exited),
        Look::Behind => Ok(Standing::Behind),
        Look::Elsewhere => Err(Error::tagged(
            "target_changed",
            format!(
                "pid {} no longer runs in pane {pane}; {untouched}",
                agent.pid
            ),
        )),
        Look::Front {
            tty,
            children,
            other,
        } => {
            let pids: Vec<i64> = children.iter().map(|p| p.pid).collect();
            let reading = discovery::reading_tty(&pids, tty).map_err(|e| {
                Error::tagged(
                    "io",
                    format!("who reads {tty} cannot be told: {e}; {untouched}"),
                )
            })?;
            match (children.iter().find(|p| reading.contains(&p.pid)), other) {
                (Some(p), _) => Err(Error::tagged_for(
                    "busy",
                    EDITOR_OPEN,
                    in_front(p, ", reading its terminal (its external editor?)"),
                )),
                (None, Some(p)) => others(p),
                (None, None) => Ok(Standing::Owns),
            }
        }
        Look::Covered(p) => Err(Error::tagged_for(
            "busy",
            EDITOR_OPEN,
            in_front(p, ", started by it (its external editor?)"),
        )),
        Look::Job(p) => Err(Error::tagged_for(
            "host_shared",
            JOB_IN_FRONT,
            in_front(
                p,
                "; the agent is suspended behind it, and closing would kill that job too",
            ),
        )),
        Look::Others(p) => others(p),
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

/// May the exit command be typed into `s`'s input box? Not when it holds text — a prompt
/// Escape put back, a draft: the command would go out with it as one prompt, and that prompt
/// would run — or a prompt waits in its place, which would take it as its answer; nor when the
/// screen cannot be read. The kill ends the session either way. A box fleet cannot find or
/// read (a layout it does not know) does not stop it, as it does not stop `send`.
fn box_clear(s: &Session) -> bool {
    use crate::core::input::{self, InputRead};
    matches!(
        input::of(s),
        Ok(InputRead::Empty | InputRead::NoBox | InputRead::Unclear(_))
    )
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

/// Wait (≤ `timeout`) for the agent to exit; whether it did (a `ps` that fails says not).
fn wait_exit(agent: &Agent, timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    loop {
        if matches!(agent.alive(), Ok(false)) {
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
    use super::*;
    use crate::core::harness::Harness;

    const START: &str = "2026-10-17T13:19:29Z";

    /// A process on `ttys001` (`tty` `None`: none), started at [`START`].
    fn p(pid: i64, ppid: i64, pgid: i64, tpgid: i64, stat: &str) -> Proc {
        Proc {
            pid,
            ppid,
            pgid,
            tpgid,
            stat: stat.into(),
            tty: Some("ttys001".into()),
            start: Some(START.into()),
            command: format!("cmd{pid}"),
        }
    }

    fn agent() -> Agent {
        Agent {
            pid: 20,
            start: START.into(),
        }
    }

    const PANE: Option<&str> = Some("/dev/ttys001");

    // The shell (10) started the agent (20); its MCP server (21) shares its group.
    fn base(agent_stat: &str, tpgid: i64) -> Vec<Proc> {
        vec![
            p(10, 1, 10, tpgid, "Ss"),
            p(20, 10, 20, tpgid, agent_stat),
            p(21, 20, 20, tpgid, agent_stat),
        ]
    }

    #[test]
    fn an_agent_in_front_owns_its_pane_with_its_children_to_check() {
        let procs = base("S+", 20);
        match look(&procs, &agent(), PANE) {
            Look::Front {
                tty,
                children,
                other: None,
            } => {
                assert_eq!(tty, "ttys001");
                assert_eq!(children.iter().map(|c| c.pid).collect::<Vec<_>>(), [21]);
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn an_ancestor_in_its_own_group_is_no_child() {
        // npm's Codex: `node codex.js` (15) runs the native agent (20) in its own group, the
        // terminal its stdin too. It is the agent's ancestor, never what covers it.
        let procs = vec![
            p(10, 1, 10, 15, "Ss"),
            p(15, 10, 15, 15, "S+"),
            p(20, 15, 15, 15, "S+"),
            p(21, 20, 15, 15, "S+"),
        ];
        match look(&procs, &agent(), PANE) {
            Look::Front {
                children,
                other: None,
                ..
            } => assert_eq!(children.iter().map(|c| c.pid).collect::<Vec<_>>(), [21]),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn a_stopped_job_on_its_terminal_is_another_job() {
        // `vim` (40), Ctrl-Z, `fg %1`: the agent is in front, `vim` stopped behind it.
        let mut procs = base("S+", 20);
        procs.push(p(40, 10, 40, 20, "T"));
        match look(&procs, &agent(), PANE) {
            Look::Front { other, .. } => assert_eq!(other, Some(&procs[3])),
            other => panic!("{other:?}"),
        }
        // Both suspended, the shell at its prompt in front.
        let mut procs = base("T", 10);
        procs.push(p(40, 10, 40, 10, "T"));
        assert_eq!(look(&procs, &agent(), PANE), Look::Others(&procs[3]));
        // Not: a job running in the background (a prompt's helper), a stopped process on
        // another terminal, one the agent started, or a stopped zombie.
        let mut procs = base("S+", 20);
        procs.push(p(41, 10, 41, 20, "S"));
        let mut far = p(42, 1, 42, 42, "T");
        far.tty = Some("ttys009".into());
        procs.push(far);
        procs.push(p(43, 20, 43, 20, "T"));
        procs.push(p(44, 10, 44, 20, "Z"));
        match look(&procs, &agent(), PANE) {
            Look::Front { other: None, .. } => {}
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn what_is_in_front_is_told_before_other_jobs() {
        // An editor of its own in front, and its own process in that group too: covered.
        let mut procs = base("S", 30);
        procs.push(p(30, 20, 30, 30, "S+"));
        procs.push(p(31, 1, 30, 30, "S+"));
        procs.push(p(40, 10, 40, 30, "T"));
        assert_eq!(look(&procs, &agent(), PANE), Look::Covered(&procs[3]));
        // Another job in front, a stopped one besides.
        let mut procs = base("T", 50);
        procs.push(p(50, 10, 50, 50, "S+"));
        procs.push(p(40, 10, 40, 50, "T"));
        assert_eq!(look(&procs, &agent(), PANE), Look::Job(&procs[3]));
    }

    #[test]
    fn its_pid_as_another_process_is_exited_not_changed() {
        // A reused pid: same pid, same terminal, in front — but started later.
        let mut procs = base("S+", 20);
        procs[1].start = Some("2026-10-17T13:20:00Z".into());
        assert_eq!(look(&procs, &agent(), PANE), Look::Exited);
        // Unreadable start, a zombie, no such pid: exited too.
        procs[1].start = None;
        assert_eq!(look(&procs, &agent(), PANE), Look::Exited);
        let mut procs = base("Z+", 20);
        assert_eq!(look(&procs, &agent(), PANE), Look::Exited);
        procs.remove(1);
        assert_eq!(look(&procs, &agent(), PANE), Look::Exited);
    }

    #[test]
    fn off_its_panes_terminal_is_elsewhere() {
        let procs = base("S+", 20);
        assert_eq!(
            look(&procs, &agent(), Some("/dev/ttys009")),
            Look::Elsewhere
        );
        assert_eq!(look(&procs, &agent(), None), Look::Elsewhere);
        let mut procs = procs;
        procs[1].tty = None;
        assert_eq!(look(&procs, &agent(), PANE), Look::Elsewhere);
    }

    #[test]
    fn an_editor_in_a_group_of_its_own_covers_it() {
        let mut procs = base("S", 30);
        procs.push(p(30, 20, 30, 30, "S+"));
        assert_eq!(look(&procs, &agent(), PANE), Look::Covered(&procs[3]));
    }

    #[test]
    fn suspended_behind_another_job_is_that_job() {
        let mut procs = base("T", 40);
        procs.push(p(40, 10, 40, 40, "S+"));
        assert_eq!(look(&procs, &agent(), PANE), Look::Job(&procs[3]));
    }

    #[test]
    fn suspended_behind_its_own_group_or_its_shells_prompt_is_behind() {
        // No job control: it stopped in front.
        assert_eq!(look(&base("T+", 20), &agent(), PANE), Look::Behind);
        // Ctrl-Z: the shell that started it is in front, at its prompt.
        assert_eq!(look(&base("T", 10), &agent(), PANE), Look::Behind);
        // Running in the background, the shell in front.
        assert_eq!(look(&base("S", 10), &agent(), PANE), Look::Behind);
    }

    #[test]
    fn every_harness_has_a_plan() {
        assert_eq!(Harness::Claude.exit_command(), Some("/exit"));
        assert_eq!(Harness::Codex.exit_command(), Some("/quit"));
        assert_eq!(Harness::Pi.exit_command(), None);
    }
}
