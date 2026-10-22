//! What `fleet model` and `fleet effort` share: type one of Claude Code's switching slash
//! commands (`/model <id>`, `/effort <level>`) into a session, as typed in its terminal, and
//! answer the one question that can come back — its confirmation while the prompt cache is
//! warm — under one typing lock, then read the outcome off the transcript. `fleet send` is not
//! enough for that: it returns before Claude Code acts, so confirming would take a second
//! command (`answer`) and the caller still could not tell whether the switch happened.
//!
//! - **Exact target only** (`id:<session_id>` / `pid:<n>`), and **this process only**:
//!   `--expect-pid` / `--expect-start` name the process the caller saw, as for `close`
//!   (`target_changed`).
//! - **Claude Code only** (`harness_unsupported`), **in a tmux pane only**
//!   (`backend_unsupported`: the box is read with its text attributes).
//! - **Under the session's typing lock** (`busy`, `locked`, while another fleet command holds
//!   it), right before typing, in this order, and nothing is typed unless all four hold:
//!   1. the agent owns its pane, by `close`'s foreground check ([`close::standing`]): the same
//!      process, in front of its pane's terminal, nothing of its own reading in front of it —
//!      else `busy` (`editor_open`), `host_shared` (`job_in_front`, or [`close::SUSPENDED`]:
//!      suspended or in the background behind its shell, whose prompt would get the text),
//!      `target_not_found` (exited) or `target_changed`;
//!   2. its Status, read afresh as `list` reads it (Claude Code's session registry, by the
//!      agent's pid), is `idle` — else `not_idle`, with that `status`. That read must name the
//!      session the lock was taken for: the same process in the same pane, and the same
//!      session id (the lock is named after it, and `/clear` changes it under the same
//!      process) — else `target_changed`;
//!   3. the screen shows no working line over the box ([`input::claude_working`]) — the
//!      registry can lag a prompt typed in the terminal — else `not_idle`, `status: "busy"`;
//!   4. the input box reads empty, as `send --require-empty` reads it ([`input::of_screen`]:
//!      the same capture, and the caret — the terminal's cursor, or the one Claude Code paints
//!      while that is hidden — so blanks left in Claude Code's box count as text, and a box
//!      showing nothing whose caret cannot be found is `unreadable`)
//!      — else `send_held` with `send`'s `reason` and `input` (exit 3). The hold is always
//!      `--require-empty`'s: a box fleet cannot find or read holds too, since the
//!      confirmation read after the command needs to know what the box was.
//! - **Then** the command and Enter, and the screen and the transcript are read for up to
//!   [`WAIT`] — every tmux call given only what is left of it, so a tmux that stops answering
//!   ends the wait as `outcome_uncertain` rather than holding the lock. The command's
//!   confirmation for that very value ([`Shown::Confirm`]) gets the key its first choice
//!   names — once `owns` is checked again and a second read of the screen still shows that
//!   very confirmation, the lock held throughout; a confirmation gone by then (answered in the
//!   terminal) gets nothing — and nothing else is pressed: work still under way where the box
//!   was, or a passing hint banner there ([`Shown::Pending`]), is waited for; any other screen
//!   in place of the box for [`SETTLE`] is `confirm_unrecognized` — unless the transcript,
//!   read once more first, already holds the command's answer, which then decides.
//! - **The outcome** is the command's answer in the transcript, written after the command
//!   began ([`Command::answer`]); its `timestamp` is `switched_at`. No answer within the wait,
//!   and a failure to read or press anything once the command was typed, are
//!   `outcome_uncertain`. An `io` failure says in `typed` whether the command was being typed
//!   (`--json`).
//!
//! `-n` (or `FLEET_DRY_RUN=1`) runs every check before typing, the lock included, and types
//! nothing.

use std::time::{Duration, Instant};

use crate::cli::close::{self, Agent, Expect, Standing};
use crate::cli::commands::{SEND_HELD_EXIT, host_label, typed_field};
use crate::core::backend;
use crate::core::discovery::{self, Backend, Session, TargetError};
use crate::core::harness::Harness;
use crate::core::input::{self, InputRead};
use crate::core::lock::{self, LockError};
use crate::core::slash::{self, Shown};
use crate::error::{Error, Result};

/// How long after the Enter the screen and the transcript are read for the outcome.
pub const WAIT: Duration = Duration::from_secs(5);
/// The least the transcript is given after the confirmation's key, however late it came.
const AFTER_CONFIRM: Duration = Duration::from_secs(2);
/// How long a screen that is neither the box nor the confirmation has to stay before it stops
/// the switch: a redraw caught halfway is not one.
pub const SETTLE: Duration = Duration::from_millis(500);
/// How often the screen and the transcript are read.
const POLL: Duration = Duration::from_millis(100);

/// The switch holds as `send --require-empty` does, whether or not `--require-empty` is given:
/// a box fleet cannot find or read holds (`no_input_box`, `unreadable`), never types with a
/// warning. The flag is accepted so a caller can pass `send`'s and `rename`'s guard alike.
const REQUIRE_EMPTY: bool = true;

const NOTHING: &str = "nothing typed";

/// One switching command, as [`switch`] types it and reads its answer.
pub trait Command {
    /// What a switch done reports besides the session, `confirmed` and `switched_at`.
    type Done;
    /// fleet's command and Claude Code's: `model` (`/model`), `effort` (`/effort`).
    const NAME: &'static str;
    /// The words Claude Code's transcript answers a switch with (`Set model to`), for the
    /// message when none came.
    const SUCCESS: &'static str;

    /// What is typed after the command: the model id, the level.
    fn value(&self) -> &str;
    /// The confirmation it asks for, as a message names it ("Claude Code's switch
    /// confirmation for <id>").
    fn confirmation(&self) -> String;
    /// What the screen shows after the command, where the input box was.
    fn shown(&self, screen: &str, styled: bool) -> Shown;
    /// A refusal Claude Code draws under the command, with the box back: one on `now` that is
    /// not on `before` (the screen before the command). None by default.
    fn drawn(&self, _before: &str, _now: &str) -> Option<Error> {
        None
    }
    /// Transcript line `line`, the next one written since the command: its `timestamp` and
    /// what it means — the switch done, or why not — when it answers the command. `typed`
    /// says what was typed, for the messages; `confirmed`, whether fleet pressed the
    /// confirmation's key.
    fn answer(
        &mut self,
        line: &str,
        typed: &str,
        confirmed: bool,
    ) -> Option<(String, Result<Self::Done>)>;
}

/// The caller's target and guard.
pub struct Target<'a> {
    pub target: &'a str,
    /// The process the caller means (`--expect-pid` + `--expect-start`, given together).
    pub expect: Option<&'a Expect>,
}

/// A switch done (or, with `-n`, one that would be typed).
pub struct Switched<D> {
    pub session: Session,
    /// Claude Code asked to confirm, and its first choice was picked.
    pub confirmed: bool,
    /// The `timestamp` of the transcript's answer; `None` under `-n`.
    pub switched_at: Option<String>,
    /// What the command reports; `None` under `-n`.
    pub done: Option<D>,
}

/// What a failure's JSON object carries besides `kind`, `error` and `reason`.
#[derive(Default)]
pub struct Extra {
    /// `not_idle`: the Status that was not `idle` (`busy` for a working line on screen).
    status: Option<String>,
    /// `send_held`: the input box's text — sensitive, so nowhere else.
    input: Option<String>,
    /// A key went (or may have gone) to the pane: the command was being typed, or was.
    typed: bool,
}

/// A failure, printed as one JSON object under `--json` (`{ ok: false, host, kind, error }`,
/// with `extra`'s fields), and returned.
pub fn failed(e: Error, json: bool, extra: Extra) -> Result<()> {
    if json {
        let mut v = e.json();
        v["host"] = host_label().into();
        if let Some(st) = extra.status {
            v["status"] = st.into();
        }
        if let Some(text) = extra.input {
            v["input"] = text.into();
        }
        typed_field(&mut v, e.kind(), extra.typed);
        println!(
            "{}",
            serde_json::to_string_pretty(&v).unwrap_or_else(|_| v.to_string())
        );
    }
    Err(e)
}

/// Switch `t`'s session with `cmd`, as the module says; `extra` gathers what a failure's JSON
/// carries.
pub fn switch<C: Command>(
    t: &Target,
    cmd: &mut C,
    dry_run: bool,
    extra: &mut Extra,
) -> Result<Switched<C::Done>> {
    let name = C::NAME;
    let began = chrono::Utc::now();
    match discovery::exact_target(t.target) {
        None => {
            return Err(Error::tagged(
                "usage",
                format!(
                    "\"{}\": {name} takes an exact target, id:<session_id> or pid:<n> — never a fragment",
                    t.target
                ),
            ));
        }
        Some(Err(e)) => return Err(e.into()),
        Some(Ok(_)) => {}
    }
    if let Some(e) = t.expect
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
    let s = discovery::resolve_target(t.target)?;
    if s.harness != Harness::Claude {
        return Err(Error::tagged(
            "harness_unsupported",
            format!(
                "session {} runs {}: {name} switches Claude Code sessions only; {NOTHING}",
                s.label(),
                s.harness.label()
            ),
        ));
    }
    let Some(start) = close::start_of(&s) else {
        return Err(Error::tagged(
            "io",
            format!(
                "session {}: its process's start time cannot be read; {NOTHING}",
                s.label()
            ),
        ));
    };
    let agent = Agent { pid: s.pid, start };
    if let Some(e) = t.expect
        && (e.pid != agent.pid || !discovery::same_start(&e.start, &agent.start))
    {
        return Err(Error::tagged(
            "target_changed",
            format!(
                "{} is now pid {} started {}, not the expected pid {} started {}; {NOTHING}",
                t.target, agent.pid, agent.start, e.pid, e.start
            ),
        ));
    }
    let pane = match (s.backend, s.handle.as_deref()) {
        (Backend::Tmux, Some(pane)) => pane.to_string(),
        _ => {
            return Err(Error::tagged(
                "backend_unsupported",
                format!(
                    "session {} runs in {}, not a tmux pane: {name} reads the screen with its text attributes, which only tmux gives; {NOTHING}",
                    s.label(),
                    s.backend.label()
                ),
            ));
        }
    };
    let _lock = lock::try_lock(&s).map_err(|e| {
        let msg = format!("{e}; {NOTHING}");
        match e {
            LockError::Busy => Error::tagged_for("busy", close::LOCKED, msg),
            LockError::Io(_) => Error::tagged("io", msg),
        }
    })?;

    // 1. The agent owns its pane.
    owned(&agent, &pane, NOTHING)?;
    // 2. Its Status, read afresh, is idle (`shell`, a background shell running behind its
    //    prompt, included) — and it is still the session locked.
    let s = afresh(t.target, &agent, &pane, &s)?;
    if !s.is_idle() {
        extra.status = Some(s.status.clone());
        return Err(Error::tagged(
            "not_idle",
            format!(
                "session {} is {}, not idle; {NOTHING} (try again once it is idle)",
                s.label(),
                s.status
            ),
        ));
    }
    // 3. and 4., off one capture: no working line, an empty box.
    let (before, styled) = backend::peek_styled(&s)
        .map_err(|e| Error::tagged("io", format!("the screen cannot be read: {e}; {NOTHING}")))?;
    if let Some(line) = input::claude_working(&before) {
        extra.status = Some("busy".into());
        return Err(Error::tagged(
            "not_idle",
            format!(
                "session {} shows Claude Code working (`{line}`) though its status reads idle; {NOTHING} (try again once it is idle)",
                s.label()
            ),
        ));
    }
    let read = input::of_screen(&s, &before, styled);
    if let Some(reason) = read.reason().filter(|_| read.holds(REQUIRE_EMPTY)) {
        extra.input = read.text().map(str::to_string);
        return Err(Error::tagged_exit_for(
            SEND_HELD_EXIT,
            "send_held",
            reason,
            held(&read, name),
        ));
    }
    if dry_run {
        return Ok(Switched {
            session: s,
            confirmed: false,
            switched_at: None,
            done: None,
        });
    }

    // Only what the transcript gains from now on counts.
    let (home, cwd, sid) = (
        discovery::claude_home(),
        s.cwd.clone(),
        s.session_id.clone(),
    );
    let mut transcript = slash::Tail::from_end(move || {
        crate::core::context::locate_transcript(&home, cwd.as_deref(), sid.as_deref()?)
    });
    let command = format!("/{name} {}", cmd.value());
    extra.typed = true;
    backend::send(&s, &command).map_err(|e| {
        Error::tagged(
            "io",
            format!("{e}; `{command}` may be partly typed, nothing pressed after it"),
        )
    })?;

    let typed = format!("`{command}` was typed");
    let mut deadline = Instant::now() + WAIT;
    let mut confirmed = false;
    let mut other_since: Option<Instant> = None;
    // The answer the transcript gained since it was last read, once one came.
    let mut answered = |cmd: &mut C, confirmed: bool| -> Option<(String, Result<C::Done>)> {
        transcript
            .read()
            .iter()
            .filter_map(|line| cmd.answer(line, &typed, confirmed))
            .find(|(at, _)| slash::at_or_after(at, began))
    };
    let done = |(at, r): (String, Result<C::Done>), session: Session, confirmed: bool| {
        r.map(|d| Switched {
            session,
            confirmed,
            switched_at: Some(at),
            done: Some(d),
        })
    };
    loop {
        if let Some(a) = answered(cmd, confirmed) {
            return done(a, s, confirmed);
        }
        // Every tmux call from here gets only what is left of the wait.
        let left = |deadline: Instant| deadline.saturating_duration_since(Instant::now());
        if left(deadline).is_zero() {
            break;
        }
        if let Ok((screen, styled)) = backend::peek_styled_within(&s, left(deadline)) {
            match cmd.shown(&screen, styled) {
                Shown::Confirm { key } if !confirmed => {
                    other_since = None;
                    owned_within(
                        &agent,
                        &pane,
                        &format!("{typed}, its confirmation is on screen, nothing pressed"),
                        Some(left(deadline)),
                    )
                    .map_err(|e| after_typing(e, &typed))?;
                    // Read again: the confirmation can be answered in the terminal while `owns`
                    // looks, and the key would then land in the box. Only that very
                    // confirmation, still there, gets it; else the loop reads on.
                    let still = backend::peek_styled_within(&s, left(deadline)).is_ok_and(
                        |(screen, styled)| {
                            cmd.shown(&screen, styled) == Shown::Confirm { key: key.clone() }
                        },
                    );
                    if !still {
                        std::thread::sleep(POLL.min(left(deadline)));
                        continue;
                    }
                    backend::press_within(&s, &key, left(deadline)).map_err(|e| {
                        Error::tagged(
                            "outcome_uncertain",
                            format!(
                                "{e}; {typed}, and its confirmation's key may or may not have been pressed: whether it switched cannot be told (see `fleet peek`)"
                            ),
                        )
                    })?;
                    confirmed = true;
                    deadline = deadline.max(Instant::now() + AFTER_CONFIRM);
                }
                Shown::Other if !confirmed => {
                    let since = *other_since.get_or_insert_with(Instant::now);
                    if since.elapsed() >= SETTLE {
                        // The transcript wins: a screen caught between the answer and the box
                        // coming back (a hint banner in its place) is no refusal of a switch
                        // already written.
                        if let Some(a) = answered(cmd, confirmed) {
                            return done(a, s, confirmed);
                        }
                        return Err(Error::tagged(
                            "confirm_unrecognized",
                            format!(
                                "{typed}, and the screen shows something other than {} or its input box; nothing pressed (see `fleet peek`)",
                                cmd.confirmation()
                            ),
                        ));
                    }
                }
                Shown::Box => {
                    other_since = None;
                    if let Some(e) = cmd.drawn(&before, &screen) {
                        return Err(e);
                    }
                }
                // Work under way where the box was ("Running PreModelSwitch hooks…", a hint
                // banner), however long it takes; the confirmation still on screen just after
                // its key, or anything once it was pressed: what comes next, or the
                // transcript, decides.
                Shown::Pending | Shown::Confirm { .. } | Shown::Other => other_since = None,
            }
        }
        std::thread::sleep(POLL.min(deadline.saturating_duration_since(Instant::now())));
    }
    Err(Error::tagged(
        "outcome_uncertain",
        format!(
            "{typed}{}, but Claude Code wrote no \"{}\" in its transcript within {}s: whether it switched cannot be told (see `fleet peek`)",
            if confirmed {
                " and its confirmation answered"
            } else {
                ""
            },
            C::SUCCESS,
            WAIT.as_secs()
        ),
    ))
}

/// A failure once the command was typed: a refusal keeps its kind (the agent no longer owns
/// its pane, …); one to read or ask anything (`io`: a tmux call that failed, or gave no answer
/// within the wait) leaves the outcome unknown — `outcome_uncertain`.
fn after_typing(e: Error, typed: &str) -> Error {
    if e.kind() != "io" {
        return e;
    }
    Error::tagged(
        "outcome_uncertain",
        format!("{e}; {typed}: whether it switched cannot be told (see `fleet peek`)"),
    )
}

/// [`close::standing`] for typing: only an agent that owns its pane gets anything typed.
/// Suspended or in the background behind its shell's prompt is `host_shared`
/// ([`close::SUSPENDED`]) — that prompt would get the text — and gone is `target_not_found`.
fn owned(agent: &Agent, pane: &str, untouched: &str) -> Result<()> {
    owned_within(agent, pane, untouched, None)
}

/// [`owned`], its tmux call given at most `limit` when one is given.
fn owned_within(agent: &Agent, pane: &str, untouched: &str, limit: Option<Duration>) -> Result<()> {
    match close::standing_within(agent, pane, untouched, false, limit)? {
        Standing::Owns => Ok(()),
        Standing::Behind => Err(Error::tagged_for(
            "host_shared",
            close::SUSPENDED,
            format!(
                "pid {} is suspended or in the background in pane {pane}, its shell in front: what is typed would go to the shell; {untouched}",
                agent.pid
            ),
        )),
        Standing::Exited => Err(Error::tagged(
            "target_not_found",
            format!("pid {} has exited; {untouched}", agent.pid),
        )),
    }
}

/// The session `target` names now, read afresh as `list` reads it (Claude Code's registry):
/// still this agent, in pane `pane`, and still the session `locked` (the one whose lock is
/// held: its id — `/clear` gives the same process a new one, whose lock another fleet command
/// could take) — else `target_changed`, or `target_not_found` when it is gone. Looked up by
/// the agent's pid, whatever `target` is: an `id:` target names the old id, which a `/clear`
/// leaves to no session, and that is `target_changed` as for `send`, not `target_not_found`.
fn afresh(target: &str, agent: &Agent, pane: &str, locked: &Session) -> Result<Session> {
    let s = discovery::resolve_target(&format!("pid:{}", agent.pid)).map_err(|e| match e {
        TargetError::NotFound(m) => Error::tagged("target_not_found", format!("{m}; {NOTHING}")),
        e => Error::tagged(
            "target_changed",
            format!("{target} no longer resolves to it: {e}; {NOTHING}"),
        ),
    })?;
    let same = s.pid == agent.pid
        && close::start_of(&s).is_some_and(|st| discovery::same_start(&st, &agent.start))
        && s.handle.as_deref() == Some(pane);
    if !same {
        return Err(Error::tagged(
            "target_changed",
            format!(
                "{target} is now pid {} in {}; {NOTHING}",
                s.pid,
                s.handle.as_deref().unwrap_or("no pane")
            ),
        ));
    }
    if lock::lock_path(&s) != lock::lock_path(locked) {
        let id = |s: &Session| s.session_id.clone().unwrap_or_else(|| "none".into());
        return Err(Error::tagged(
            "target_changed",
            format!(
                "{target} now runs session {}, not {} (its id changed, as `/clear` changes it, since it was looked up); {NOTHING}",
                id(&s),
                id(locked)
            ),
        ));
    }
    Ok(s)
}

/// Why `/<name>` was not typed into the box `read` found, and what to do in the terminal —
/// never with the box's text.
fn held(read: &InputRead, name: &str) -> String {
    match read {
        InputRead::Typed(_) => format!(
            "the input box holds text — `/{name}` would go out with it as one prompt; nothing typed (send or clear it in the terminal, then try again)"
        ),
        InputRead::Prompt => "a prompt waits for its answer in place of the input box; nothing typed (answer it, then try again)".into(),
        InputRead::NoBox => "fleet cannot find the input box on screen (a menu or the transcript view is open, or a layout it does not know); nothing typed (close it with Escape, then try again)".into(),
        InputRead::Unclear(_) => format!(
            "the input box cannot be read; nothing typed (switch with /{name} in the terminal instead)"
        ),
        InputRead::Empty => "the input box is empty".into(),
    }
}

/// A switch declined in the terminal (`Kept … as <kept>`): the setting is unchanged. Still
/// `outcome_uncertain` (the kinds are closed), with fleet's key, if it pressed one, come too
/// late. `what` names the setting (`model`, `effort level`).
pub fn kept_error(typed: &str, what: &str, kept: Option<&str>, confirmed: bool) -> Error {
    let kept = kept.map_or_else(|| format!("its {what}"), str::to_string);
    Error::tagged(
        "outcome_uncertain",
        if confirmed {
            format!(
                "{typed} and its confirmation's first choice pressed, but the switch was declined in the terminal before that key arrived: Claude Code kept {kept}, the {what} is unchanged (the key may be in its input box: see `fleet peek`)"
            )
        } else {
            format!(
                "{typed}, but the switch was declined in the terminal: Claude Code kept {kept}, the {what} is unchanged"
            )
        },
    )
}
