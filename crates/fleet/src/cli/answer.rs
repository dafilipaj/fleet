//! `fleet answer <target>`: answer the prompt a session is waiting on, by typing what a
//! human would — Claude Code's `AskUserQuestion` (`--question`), a Claude Code permission
//! prompt or a Codex approval prompt (`--choice`). `--list` says what is on screen.
//!
//! Nothing is typed unless the terminal shows that very prompt, untouched (the cursor still
//! on its first answer, nothing ticked or typed), at the bottom of the screen. A refusal says
//! which check failed as a code (`detail`, a [`Detail`]) beside the pane's size: never screen
//! or transcript text, so a caller can log it. And no key is typed blind:
//!
//! - **Exclusive input.** A terminal has no answer bound to a prompt id, so the sources of a
//!   race are closed instead: no human may be attached to the session's tmux session (unless
//!   `--allow-attached`), and a per-session lock keeps every other fleet command that types
//!   (`answer`, `send`, `keys`, `close`, every rename) out for the whole read → type → verify sequence.
//! - A permission prompt is answered with **one key** where it takes one (Claude Code: the
//!   answer's number; Codex: a shortcut no other answer shares), right after reading the screen.
//! - Otherwise each key is checked for: before the next one the screen must show exactly the
//!   state the keys so far lead to (cursor row, ticked boxes, typed text, answered tabs; before
//!   the final Enter the review screen listing every question with exactly the answer planned).
//!   Anything else stops it: `prompt_changed`.
//! - **After** the last key the screen is read again: the prompt must be gone, and no prompt
//!   with the same answers — nor a permission prompt that cannot be read whole, which may be
//!   either — may have taken its place within a moment. Otherwise the outcome is reported as
//!   uncertain (`outcome_uncertain`), never as success.
//!
//! What remains (docs/cli.md, "the approval race"): a terminal has no conditional key, so a
//! client attaching after the check, or anything else writing to the pane, between the last
//! read and the key can put another prompt under it. The post-key check reports only a prompt
//! with the same answers, or a permission prompt it cannot read whole, still on screen after
//! the key (another one following the one answered). The plain case — the read prompt
//! answered by someone else, the key landing on the lookalike that replaced it, nothing after
//! — comes back as `answered: true`: neither the screen nor the transcript ties a key to a
//! prompt, so it is not detected, nor undone.

use std::time::{Duration, Instant};

use colored::Colorize;
use serde_json::{Value, json};

use crate::cli::messages::{self, Failure};
use crate::core::backend;
use crate::core::discovery::{self, Backend, Session};
use crate::core::harness::Harness;
use crate::core::lock::{self, LockError, SessionLock};
use crate::core::prompt::{self, Detail, PromptRead, ScreenPrompt};
use crate::core::questions::{self, At, Planned, Question, Step};
use crate::core::transcript::{self, Kind};
use crate::error::{Error, Result};

/// How long the screen may take to show what a key did.
const SETTLE: Duration = Duration::from_millis(1500);
/// After the prompt is gone, how long the screen is watched for one with the same answers, or
/// a permission prompt that cannot be read whole, taking its place.
const WATCH: Duration = Duration::from_millis(300);
/// How often the screen is read while waiting for it.
const POLL: Duration = Duration::from_millis(40);
/// Scrollback read with the screen: the top of a tall permission prompt.
const HISTORY_LINES: usize = 1000;
/// How far back the transcript is searched for the question named by `--question`.
const QUESTION_LOOKBACK: usize = 500;

pub struct Opts {
    pub target: String,
    pub json: bool,
    pub list: bool,
    pub choice: Option<String>,
    pub prompt: Option<String>,
    pub question: Option<String>,
    pub answers: Option<String>,
    pub allow_attached: bool,
}

enum Mode {
    List,
    Choice { n: usize, prompt: Option<String> },
    Question { id: String, answers: Value },
}

/// A failure: its kind and message, and for a refusal by a screen check, why as a code
/// (`detail`, from [`Detail`]: never screen or transcript text, so a caller can log it), plus
/// the session's pane size, filled in once the target is known.
struct Refusal {
    kind: &'static str,
    message: String,
    detail: Option<Detail>,
    pane: Option<(u32, u32)>,
    /// A key reached the pane before it (`--json`'s `typed`, on an `io` failure).
    typed: bool,
}

impl From<Failure> for Refusal {
    fn from(f: Failure) -> Self {
        Refusal {
            kind: f.kind,
            message: f.message,
            detail: None,
            pane: None,
            typed: false,
        }
    }
}

type R<T> = std::result::Result<T, Refusal>;

fn fail<T>(kind: &'static str, message: impl Into<String>) -> R<T> {
    Err(Failure {
        kind,
        message: message.into(),
    }
    .into())
}

/// Refuse for `detail`, under the kind it is reported as ([`Detail::kind`]).
fn refuse<T>(detail: Detail, message: impl Into<String>) -> R<T> {
    Err(Refusal {
        kind: detail.kind(),
        message: message.into(),
        detail: Some(detail),
        pane: None,
        typed: false,
    })
}

/// `{ cols, rows }`, or `null` when the backend does not say.
fn pane_json(size: Option<(u32, u32)>) -> Value {
    size.map_or(
        Value::Null,
        |(cols, rows)| json!({ "cols": cols, "rows": rows }),
    )
}

fn mode(o: &Opts) -> R<Mode> {
    let given = [o.list, o.choice.is_some(), o.question.is_some()]
        .iter()
        .filter(|g| **g)
        .count();
    if given != 1 {
        return fail(
            "usage",
            "give one of --list, --choice <n>, --question <tool_use_id> --answers <json>",
        );
    }
    if o.answers.is_some() != o.question.is_some() {
        return fail("usage", "--answers goes with --question (and it needs one)");
    }
    if o.prompt.is_some() && o.choice.is_none() {
        return fail("usage", "--prompt goes with --choice");
    }
    if o.list {
        return Ok(Mode::List);
    }
    if let Some(c) = &o.choice {
        return match c.trim().parse::<usize>() {
            Ok(n) if n >= 1 => Ok(Mode::Choice {
                n,
                prompt: o.prompt.clone(),
            }),
            _ => fail(
                "usage",
                format!("--choice takes an answer's number, not {c:?}"),
            ),
        };
    }
    let id = o.question.clone().unwrap_or_default();
    let answers =
        serde_json::from_str(o.answers.as_deref().unwrap_or("")).map_err(|e| Failure {
            kind: "answer_invalid",
            message: format!("--answers is not JSON: {e}"),
        })?;
    Ok(Mode::Question { id, answers })
}

fn resolve(target: &str) -> R<Session> {
    Ok(discovery::resolve_target(target).map_err(|e| Failure {
        kind: e.kind(),
        message: e.to_string(),
    })?)
}

fn drivable(s: &Session) -> R<()> {
    if discovery::is_fixture() || s.handle.is_none() || s.backend == Backend::Unknown {
        return fail(
            "backend_unsupported",
            format!(
                "{} has no terminal fleet can read and type into (backend {})",
                s.headline(),
                s.backend.label()
            ),
        );
    }
    Ok(())
}

/// What the session's terminal shows now, with scrollback above it.
fn screen(s: &Session) -> R<String> {
    drivable(s)?;
    Ok(
        backend::peek_history(s, HISTORY_LINES).map_err(|e| Failure {
            kind: "io",
            message: format!("cannot read the terminal: {e}"),
        })?,
    )
}

/// The clients attached to the session's tmux session (a human who could be typing).
fn attached_clients(s: &Session) -> R<usize> {
    match s.backend {
        Backend::Tmux => {
            let handle = s.handle.as_deref().unwrap_or_default();
            let out = crate::core::tools::tmux_cmd()
                .args(["list-clients", "-t", handle, "-F", "#{client_tty}"])
                .output()
                .map_err(|e| Failure {
                    kind: "io",
                    message: format!("tmux list-clients: {e}"),
                })?;
            if !out.status.success() {
                return fail(
                    "io",
                    format!(
                        "tmux list-clients: {}",
                        String::from_utf8_lossy(&out.stderr).trim()
                    ),
                );
            }
            Ok(String::from_utf8_lossy(&out.stdout)
                .lines()
                .filter(|l| !l.trim().is_empty())
                .count())
        }
        // An iTerm tab is a terminal on screen: a human may always be at it.
        _ => Ok(1),
    }
}

/// Exclusive input for the whole answer: nobody attached (unless allowed), the lock held.
fn exclusive(s: &Session, allow_attached: bool) -> R<SessionLock> {
    drivable(s)?;
    if !allow_attached {
        let n = attached_clients(s)?;
        if n > 0 {
            return fail(
                "human_attached",
                format!(
                    "{n} terminal client(s) attached to the session — a human could be typing into it; nothing typed (--allow-attached to answer anyway)"
                ),
            );
        }
    }
    let held = lock::try_lock(s).map_err(|e| match e {
        LockError::Busy => Failure {
            kind: "busy",
            message: format!("{e}; nothing typed"),
        },
        LockError::Io(_) => Failure {
            kind: "io",
            message: e.to_string(),
        },
    })?;
    lock::still_named(s, "nothing typed").map_err(|e| Failure {
        kind: e.kind(),
        message: e.to_string(),
    })?;
    Ok(held)
}

/// Read the screen until `ok` holds, for at most `within`.
fn wait_for(s: &Session, within: Duration, mut ok: impl FnMut(&str) -> bool) -> R<bool> {
    let deadline = Instant::now() + within;
    loop {
        if ok(&screen(s)?) {
            return Ok(true);
        }
        if Instant::now() >= deadline {
            return Ok(false);
        }
        std::thread::sleep(POLL);
    }
}

const SAME_ANSWERS: &str =
    "another prompt with the same answers is on screen now — the key may have answered it instead";

/// Did the key land? The prompt must leave the screen, and nothing that may be it or look like
/// it (`unsure`: why not) may take its place within [`WATCH`]. `None`: yes; `Some(why)`:
/// uncertain. A screen that cannot be read (the pane or tmux gone) proves nothing either way:
/// uncertain too.
fn landed(
    s: &Session,
    still: impl Fn(&str) -> bool,
    unsure: impl Fn(&str) -> Option<String>,
) -> Option<String> {
    let unread = |f: Refusal| Some(format!("{} after the answer", f.message));
    match wait_for(s, SETTLE, |screen| !still(screen)) {
        Err(f) => return unread(f),
        Ok(false) => return Some("the prompt is still on screen after the answer".into()),
        Ok(true) => {}
    }
    let mut why = None;
    match wait_for(s, WATCH, |screen| {
        why = unsure(screen);
        why.is_some()
    }) {
        Err(f) => unread(f),
        Ok(_) => why,
    }
}

/// A screen that cannot be read once keys have gone out: say how many did.
fn after_keys(done: usize, total: usize) -> impl Fn(Refusal) -> Refusal {
    move |f| Refusal {
        message: format!("{}; stopped after {done} of {total} keys", f.message),
        typed: f.typed || done > 0,
        ..f
    }
}

fn press(s: &Session, step: &Step, done: usize, total: usize) -> R<()> {
    let r = match step {
        Step::Key(k) => backend::press(s, k),
        Step::Text(t) => backend::type_literal(s, t),
    };
    r.or_else(|e| {
        fail(
            "send_failed",
            format!("typing failed after {done} of {total} keystrokes: {e}"),
        )
    })
}

fn steps_json(steps: &[Step]) -> Value {
    Value::Array(
        steps
            .iter()
            .map(|s| match s {
                Step::Key(k) => json!({ "key": k }),
                Step::Text(t) => json!({ "text": t }),
            })
            .collect(),
    )
}

fn base(s: &Session) -> serde_json::Map<String, Value> {
    let mut m = serde_json::Map::new();
    m.insert("ok".into(), json!(true));
    m.insert("session_id".into(), json!(s.session_id));
    m.insert("harness".into(), json!(s.harness));
    m
}

/// The result after typing: `answered: true` when the key verifiably landed; with a reason,
/// the `outcome_uncertain` failure (keys were typed — what they did is not certain). Under
/// `-n` / `--dry-run`: what would be typed (`dry_run: true`, `answered: false`).
fn outcome(
    s: &Session,
    prompt_kind: &str,
    steps: &[Step],
    fields: Vec<(&str, Value)>,
    dry: bool,
    uncertain: Option<String>,
) -> Value {
    let mut m = base(s);
    if let Some(why) = uncertain {
        m.insert("ok".into(), json!(false));
        m.insert("kind".into(), json!("outcome_uncertain"));
        m.insert(
            "error".into(),
            json!(format!("{why}; look at the terminal")),
        );
        // Not a refusal by a screen check: no code. Every failure object has both keys.
        m.insert("detail".into(), Value::Null);
        m.insert("pane".into(), pane_json(backend::pane_size(s)));
        m.insert("screen".into(), on_screen(s));
    } else {
        m.insert("answered".into(), json!(!dry));
        if dry {
            m.insert("dry_run".into(), json!(true));
        }
    }
    m.insert("prompt_kind".into(), json!(prompt_kind));
    for (k, v) in fields {
        m.insert(k.into(), v);
    }
    m.insert("steps".into(), steps_json(steps));
    Value::Object(m)
}

/// What `--list` would say about the screen now (best effort: `null` when unreadable).
fn on_screen(s: &Session) -> Value {
    screen(s)
        .and_then(|screen| read_prompt(s, &screen))
        .map_or(Value::Null, |(prompt, _)| prompt)
}

fn permission_json(p: &ScreenPrompt) -> Value {
    json!({
        "kind": "permission",
        "id": p.id,
        "title": p.title,
        "detail": p.detail,
        "choices": p.choices,
        "selected": p.selected,
        "untouched": p.selected == 1,
    })
}

/// The pending questions of a Claude session, newest first.
fn pending_questions(s: &Session) -> R<Vec<transcript::Entry>> {
    let Some(path) = messages::locate(s)? else {
        return Ok(Vec::new());
    };
    let page = transcript::latest(&path, s.harness, QUESTION_LOOKBACK).map_err(|e| Failure {
        kind: "io",
        message: format!("cannot read {}: {e}", path.display()),
    })?;
    Ok(page
        .entries
        .into_iter()
        .rev()
        .filter(|e| e.kind == Kind::Question && e.status == Some("pending"))
        .collect())
}

/// Does any of the questions have options with previews? Claude Code draws those beside the
/// options, a layout fleet does not read off the screen: never answered, never matched.
fn has_previews(qs: &[Question]) -> bool {
    qs.iter().any(|q| q.options.iter().any(|o| o.preview))
}

/// `--list`'s question form not all of which is on screen (`clipped`): which question it is
/// is not told, even when what is visible reads as a pending one — the same object whichever
/// check found it cut.
fn clipped_question() -> Value {
    json!({
        "kind": "question", "incomplete": true, "id": null, "questions": null,
        "active": null, "untouched": false,
    })
}

/// The prompt `screen` ends in, as `--list` reports it, and why it cannot be answered now
/// (`None`: it can — shown whole, untouched; the screen checks of `--choice` / `--question`,
/// not the attached-client, lock or transcript ones).
fn read_prompt(s: &Session, screen: &str) -> R<(Value, Option<Detail>)> {
    Ok(match prompt::permission_prompt(screen, s.harness) {
        PromptRead::Prompt(p) => {
            let why = (p.selected != 1).then_some(Detail::CursorMoved);
            (permission_json(&p), why)
        }
        PromptRead::Incomplete(detail, reason) => (
            json!({ "kind": "permission", "incomplete": true, "reason": reason }),
            Some(detail),
        ),
        PromptRead::None => match prompt::read_form(screen) {
            Err(Detail::NoForm) => (Value::Null, Some(Detail::NoForm)),
            // A form, but not all of it on screen: which question it is cannot be told.
            Err(detail) => (clipped_question(), Some(detail)),
            Ok(form) => {
                // Which question it is comes from the transcript (Claude Code only).
                let pending = if s.harness == Harness::Claude {
                    pending_questions(s)?
                } else {
                    Vec::new()
                };
                question_on_screen(&form, &pending)
            }
        },
    })
}

/// The `--list` report of a question form on screen: the pending question (newest first) it
/// shows, checked whole, and why that one cannot be answered now. With none, why not, against
/// the newest pending question: the one an answer would be for. A form found cut off
/// (`clipped`, by any check) is [`clipped_question`], never a question's id.
fn question_on_screen(
    form: &prompt::Form,
    pending: &[transcript::Entry],
) -> (Value, Option<Detail>) {
    let found = pending.iter().find_map(|e| {
        let qs = e.questions.as_ref().filter(|qs| !has_previews(qs))?;
        let active = form.shows(qs).ok()?;
        Some((e, qs, active))
    });
    if let Some((e, qs, active)) = found {
        let why = form.ready(qs).err().map(|(detail, _)| detail);
        if why == Some(Detail::Clipped) {
            return (clipped_question(), why);
        }
        return (
            json!({
                "kind": "question",
                "id": e.id,
                "questions": qs,
                "active": active,
                "untouched": active == 0 && form.untouched(&qs[0]),
            }),
            why,
        );
    }
    let why = match pending.first().and_then(|e| e.questions.as_deref()) {
        None => Detail::DifferentPrompt,
        Some(qs) if has_previews(qs) => Detail::LayoutUnsupported,
        Some(qs) => form
            .check(qs)
            .err()
            .map_or(Detail::DifferentPrompt, |(detail, _)| detail),
    };
    if why == Detail::Clipped {
        return (clipped_question(), Some(why));
    }
    (
        json!({
            "kind": "question", "id": null, "questions": null,
            "active": null, "untouched": false,
        }),
        Some(why),
    )
}

fn list(s: &Session) -> R<Value> {
    // The screen is read once; the pane size is one more tmux call, no second capture.
    let screen = screen(s)?;
    let (prompt, why) = read_prompt(s, &screen)?;
    let mut m = base(s);
    m.insert("prompt".into(), prompt);
    m.insert("answerable".into(), json!(why.is_none()));
    m.insert("detail".into(), json!(why.map(Detail::code)));
    m.insert("pane".into(), pane_json(backend::pane_size(s)));
    Ok(Value::Object(m))
}

/// The permission prompt on screen, or why there is none to answer.
fn read_permission(s: &Session) -> R<ScreenPrompt> {
    let screen = screen(s)?;
    match prompt::permission_prompt(&screen, s.harness) {
        PromptRead::Prompt(p) => Ok(p),
        PromptRead::Incomplete(detail, why) => refuse(detail, format!("{why}; nothing typed")),
        PromptRead::None if prompt::is_question_form(&screen) => refuse(
            Detail::DifferentPrompt,
            "the terminal shows a question form — answer it with --question; nothing typed",
        ),
        PromptRead::None => refuse(
            Detail::NoForm,
            "the terminal is not showing a permission prompt; nothing typed",
        ),
    }
}

fn same_answers(a: &ScreenPrompt, b: &ScreenPrompt) -> bool {
    a.choices.len() == b.choices.len()
        && a.choices
            .iter()
            .zip(&b.choices)
            .all(|(x, y)| x.label == y.label)
}

/// `prompt_changed`: the screen did not reach the state the keys so far lead to; `detail` what
/// it showed instead, the last time it was read.
fn changed<T>(detail: Detail, message: String) -> R<T> {
    Err(Refusal {
        kind: "prompt_changed",
        message,
        detail: Some(detail),
        pane: None,
        typed: false,
    })
}

fn choose(s: &Session, n: usize, want: Option<&str>, allow_attached: bool, dry: bool) -> R<Value> {
    // A dry run checks this too: it says whether the answer could be typed now.
    let _lock = exclusive(s, allow_attached)?;
    let p = read_permission(s)?;
    if want.is_some_and(|w| w != p.id) {
        return refuse(
            Detail::DifferentPrompt,
            format!(
                "the terminal shows a different prompt now ({}: \"{}\"); nothing typed",
                p.id, p.title
            ),
        );
    }
    if p.selected != 1 {
        return refuse(
            Detail::CursorMoved,
            "someone moved the prompt's cursor (it is not on answer 1) — answer it in the terminal; nothing typed",
        );
    }
    let Some(choice) = p.choices.iter().find(|c| c.n == n).cloned() else {
        return fail(
            "choice_invalid",
            format!("the prompt has answers 1–{}, not {n}", p.choices.len()),
        );
    };
    let (method, steps) = match choice.key.as_deref() {
        // One key, chosen from the screen just read: no cursor movement to race.
        Some(k) => ("key", vec![Step::Key(k.to_string())]),
        None => {
            let mut steps: Vec<Step> =
                std::iter::repeat_n(Step::Key("Down".into()), n - 1).collect();
            steps.push(Step::Key("Enter".into()));
            ("cursor", steps)
        }
    };
    let fields = vec![
        ("prompt", permission_json(&p)),
        ("choice", json!(choice)),
        ("method", json!(method)),
    ];
    if dry {
        return Ok(outcome(s, "permission", &steps, fields, true, None));
    }
    let total = steps.len();
    for (i, step) in steps.iter().enumerate() {
        if i > 0 {
            // The cursor must have moved to exactly answer i+1 of this very prompt; before the
            // Enter, the whole prompt is read again.
            let expect = i + 1;
            let mut seen = Detail::NoForm;
            let ok = wait_for(s, SETTLE, |screen| {
                match prompt::permission_prompt(screen, s.harness) {
                    PromptRead::Prompt(now) if now.id == p.id && now.selected == expect => {
                        return true;
                    }
                    PromptRead::Prompt(now) if now.id == p.id => seen = Detail::CursorMoved,
                    PromptRead::Prompt(_) => seen = Detail::DifferentPrompt,
                    PromptRead::Incomplete(detail, _) => seen = detail,
                    PromptRead::None if prompt::is_question_form(screen) => {
                        seen = Detail::DifferentPrompt;
                    }
                    PromptRead::None => seen = Detail::NoForm,
                }
                false
            })
            .map_err(after_keys(i, total))?;
            if !ok {
                return changed(
                    seen,
                    format!(
                        "the prompt changed while answering (expected the cursor on answer {expect}); stopped after {i} of {total} keys"
                    ),
                );
            }
        }
        press(s, step, i, total)?;
    }
    let uncertain = landed(
        s,
        |screen| matches!(prompt::permission_prompt(screen, s.harness), PromptRead::Prompt(now) if now.id == p.id),
        // A prompt not read whole proves nothing: it may be this one, or a queued one with the
        // same answers — never "no prompt".
        |screen| match prompt::permission_prompt(screen, s.harness) {
            PromptRead::Prompt(now) if same_answers(&now, &p) => Some(SAME_ANSWERS.into()),
            PromptRead::Incomplete(_, why) => Some(format!(
                "a prompt that cannot be read whole is on screen now ({why}) — it may be the one answered, or one the key answered instead"
            )),
            PromptRead::Prompt(_) | PromptRead::None => None,
        },
    );
    Ok(outcome(s, "permission", &steps, fields, false, uncertain))
}

/// Does the screen show the form in state `at` of answering `qs`? `Err`: what it shows instead
/// — no form or not all of it, another form, or this one in another state (the cursor
/// elsewhere: `CursorMoved`; other boxes, text, answered tabs or answers on the review:
/// `Touched`).
fn form_at(screen: &str, qs: &[Question], at: &At) -> std::result::Result<(), Detail> {
    match at {
        At::Review { expect } => {
            let Some(r) = prompt::parse_review(screen) else {
                return Err(match prompt::read_form(screen) {
                    // Still the form (or one): the Enter before the review did not land as planned.
                    Ok(_) => Detail::Touched,
                    Err(detail) => detail,
                });
            };
            if r.items != *expect
                || !(r.tabs.is_empty()
                    || (r.tabs.len() == qs.len() && r.tabs.iter().all(|(_, done)| *done)))
            {
                return Err(Detail::Touched);
            }
            if !r.submit_focused {
                return Err(Detail::CursorMoved);
            }
            Ok(())
        }
        At::Form {
            question,
            row,
            ticked,
            typed,
            done,
        } => {
            let form = prompt::read_form(screen)?;
            let active = form.check(qs).map_err(|(detail, _)| detail)?;
            if active != *question || form.cursor() != Some(*row) {
                return Err(Detail::CursorMoved);
            }
            if !form.tabs.is_empty() && form.tabs.iter().map(|(_, d)| *d).ne(done.iter().copied()) {
                return Err(Detail::Touched);
            }
            let q = &qs[*question];
            if q.multi_select {
                let mut want = ticked.clone();
                want.sort_unstable();
                if form.ticked() != want {
                    return Err(Detail::Touched);
                }
            }
            let other = form.rows.get(q.options.len());
            let as_planned = match typed {
                Some(t) => other.is_some_and(|r| r.label == *t),
                None => other.is_some_and(|r| r.label.starts_with("Type something")),
            };
            if as_planned {
                Ok(())
            } else {
                Err(Detail::Touched)
            }
        }
    }
}

fn answer_question(
    s: &Session,
    id: &str,
    answers: &Value,
    allow_attached: bool,
    dry: bool,
) -> R<Value> {
    if s.harness != Harness::Claude {
        return fail(
            "harness_unsupported",
            format!(
                "{} questions are not answerable here (Claude Code only)",
                s.harness
            ),
        );
    }
    // A dry run checks this too: it says whether the answer could be typed now.
    let _lock = exclusive(s, allow_attached)?;
    let Some(path) = messages::locate(s)? else {
        return fail("transcript_not_found", "the session has no transcript yet");
    };
    let page =
        transcript::latest(&path, Harness::Claude, QUESTION_LOOKBACK).map_err(|e| Failure {
            kind: "io",
            message: format!("cannot read {}: {e}", path.display()),
        })?;
    let questions: Vec<&transcript::Entry> = page
        .entries
        .iter()
        .filter(|e| e.kind == Kind::Question)
        .collect();
    let Some(q) = questions.iter().find(|e| e.id.as_deref() == Some(id)) else {
        return fail(
            "question_not_found",
            format!("no question {id} in this session"),
        );
    };
    match q.status {
        Some("pending") => {}
        Some("abandoned") => {
            return fail(
                "question_not_active",
                format!("question {id} was abandoned (a new prompt came before any answer)"),
            );
        }
        other => {
            return fail(
                "question_answered",
                format!("question {id} is already {}", other.unwrap_or("closed")),
            );
        }
    }
    // The one call waiting now: the same words asked under another id must not take it.
    let active: Vec<&str> = questions
        .iter()
        .filter(|e| e.status == Some("pending"))
        .filter_map(|e| e.id.as_deref())
        .collect();
    if active != [id] {
        return fail(
            "question_not_active",
            format!(
                "question {id} is not the one call waiting (waiting: {})",
                active.join(", ")
            ),
        );
    }
    if s.status != "waiting" {
        return fail(
            "not_waiting",
            format!("the session is {}, not waiting on a question", s.status),
        );
    }
    let qs = q.questions.clone().unwrap_or_default();
    let checked = questions::validate_answer(answers, &qs).map_err(|message| Failure {
        kind: "answer_invalid",
        message,
    })?;
    // The live form, whole: this question's tabs, text, options and descriptions, its head up
    // to the form's top, nothing ticked, typed or answered, the cursor on the first answer.
    let first = screen(s)?;
    let form = match prompt::read_form(&first) {
        Ok(form) => form,
        Err(Detail::NoForm)
            if !matches!(
                prompt::permission_prompt(&first, s.harness),
                PromptRead::None
            ) =>
        {
            return refuse(
                Detail::DifferentPrompt,
                "the terminal shows a permission prompt, not a question form — answer it there; nothing typed",
            );
        }
        Err(Detail::NoForm) => {
            return refuse(
                Detail::NoForm,
                "the terminal is not showing a question form — answer it there; nothing typed",
            );
        }
        Err(detail) => {
            return refuse(
                detail,
                "the question form's answers run off the captured screen (taller than the terminal) — answer it there; nothing typed",
            );
        }
    };
    if has_previews(&qs) {
        return refuse(
            Detail::LayoutUnsupported,
            "the question's options have previews, drawn beside them — fleet does not read that layout; answer it there; nothing typed",
        );
    }
    if let Err((detail, why)) = form.ready(&qs) {
        return refuse(detail, format!("{why} — answer it there; nothing typed"));
    }
    let plan: Vec<Planned> = questions::answer_plan(&qs, &checked);
    let steps: Vec<Step> = plan.iter().map(|p| p.step.clone()).collect();
    let fields = vec![("id", json!(id))];
    if dry {
        return Ok(outcome(s, "question", &steps, fields, true, None));
    }
    let total = plan.len();
    for (i, planned) in plan.iter().enumerate() {
        if i > 0 {
            let mut seen = Detail::NoForm;
            let ok = wait_for(s, SETTLE, |screen| {
                match form_at(screen, &qs, &planned.at) {
                    Ok(()) => true,
                    Err(detail) => {
                        seen = detail;
                        false
                    }
                }
            })
            .map_err(after_keys(i, total))?;
            if !ok {
                return changed(
                    seen,
                    format!(
                        "the form changed while answering (not in the state the keys so far lead to); stopped after {i} of {total} keys"
                    ),
                );
            }
        }
        press(s, &planned.step, i, total)?;
    }
    let shows_it = |screen: &str| {
        prompt::parse_form(screen).is_some_and(|f| f.shows(&qs).is_ok())
            || prompt::parse_review(screen).is_some()
    };
    let uncertain = landed(s, shows_it, |screen| {
        shows_it(screen).then(|| SAME_ANSWERS.into())
    });
    Ok(outcome(s, "question", &steps, fields, false, uncertain))
}

fn collect(o: &Opts) -> R<Value> {
    let mode = mode(o)?;
    let s = resolve(&o.target)?;
    let dry = crate::core::hosts::dry_run();
    match mode {
        Mode::List => list(&s),
        Mode::Choice { n, prompt } => choose(&s, n, prompt.as_deref(), o.allow_attached, dry),
        Mode::Question { id, answers } => answer_question(&s, &id, &answers, o.allow_attached, dry),
    }
    // Every failure once the target is known says how big its pane is.
    .map_err(|f| Refusal {
        pane: backend::pane_size(&s),
        ..f
    })
}

/// A failure of `answer --json` reported before its own checks run (a usage error clap
/// catches, a `FLEET_PATH_ONLY` refusal): the fields every answer failure carries, `null` —
/// no screen check failed, no pane is known yet.
pub fn early_failure_fields(v: &mut Value) {
    v["detail"] = Value::Null;
    v["pane"] = Value::Null;
}

pub fn run(o: Opts) -> Result<()> {
    match collect(&o) {
        // Keys went out but their outcome is not certain: a failure, with what is known.
        Ok(v) if v["ok"] == false => {
            let msg = v["error"]
                .as_str()
                .unwrap_or("outcome uncertain")
                .to_string();
            if o.json {
                println!("{}", serde_json::to_string(&v)?);
            }
            Err(Error::exit(1, msg))
        }
        Ok(v) => {
            if o.json {
                println!("{}", serde_json::to_string(&v)?);
            } else {
                print_text(&v);
            }
            Ok(())
        }
        Err(f) => {
            if o.json {
                let mut v = json!({
                    "ok": false, "kind": f.kind, "error": f.message,
                    "detail": f.detail.map(Detail::code), "pane": pane_json(f.pane),
                });
                crate::cli::commands::typed_field(&mut v, f.kind, f.typed);
                println!("{v}");
            }
            Err(Error::exit(1, f.message))
        }
    }
}

fn print_text(v: &Value) {
    let p = &v["prompt"];
    if v["dry_run"] == true {
        println!(
            "would type: {}",
            v["steps"]
                .as_array()
                .into_iter()
                .flatten()
                .map(|s| s["key"]
                    .as_str()
                    .map(str::to_string)
                    .unwrap_or_else(|| format!("{:?}", s["text"].as_str().unwrap_or(""))))
                .collect::<Vec<_>>()
                .join(" ")
        );
        return;
    }
    if v["answered"] == true {
        if v["prompt_kind"] == "permission" {
            println!(
                "answered \"{}\" with {}. {}",
                p["title"].as_str().unwrap_or(""),
                v["choice"]["n"],
                v["choice"]["label"].as_str().unwrap_or("")
            );
        } else {
            println!("answered question {}", v["id"].as_str().unwrap_or(""));
        }
        return;
    }
    if p.is_null() {
        println!("{}", "no prompt on screen".dimmed());
    } else if p["incomplete"] == true && p["kind"] == "question" {
        println!("a question form, but not all of it is on screen");
    } else if p["incomplete"] == true {
        println!(
            "a permission prompt, but {}",
            p["reason"].as_str().unwrap_or("not all of it is readable")
        );
    } else if p["kind"] == "question" {
        println!(
            "question form {}",
            p["id"].as_str().unwrap_or("(not in the transcript)")
        );
        for q in p["questions"].as_array().into_iter().flatten() {
            println!("  {}", q["question"].as_str().unwrap_or("").bold());
            for (i, o) in q["options"].as_array().into_iter().flatten().enumerate() {
                println!("    {i}: {}", o["label"].as_str().unwrap_or(""));
            }
        }
    } else {
        println!("{}", p["title"].as_str().unwrap_or("").bold());
        for d in p["detail"].as_array().into_iter().flatten() {
            println!("  {}", d.as_str().unwrap_or("").dimmed());
        }
        for c in p["choices"].as_array().into_iter().flatten() {
            let mark = if c["n"] == p["selected"] { "❯" } else { " " };
            println!("{mark} {}. {}", c["n"], c["label"].as_str().unwrap_or(""));
        }
        println!(
            "{}",
            format!("prompt {}", p["id"].as_str().unwrap_or("")).dimmed()
        );
    }
    if !p.is_null() && v["answerable"] == false {
        println!(
            "{}",
            format!(
                "not answerable now: {}",
                v["detail"].as_str().unwrap_or("unknown")
            )
            .dimmed()
        );
    }
}
