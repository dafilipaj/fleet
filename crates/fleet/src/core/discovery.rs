//! Session discovery. The authoritative source is Claude's own live registry at
//! `~/.claude/sessions/<pid>.json` (precise pid -> sessionId -> cwd -> status).
//! Each entry is enriched with the process tty and the backend it runs in.

use std::collections::HashMap;
use std::path::PathBuf;
use std::process::Command;

use serde::{Deserialize, Serialize};

use crate::core::harness::{self, Harness};

/// Claude Code's Status (2.1.308 on) for a session idle at its prompt while a background shell
/// (a `run_in_background` Bash) still runs: what it writes instead of `idle` then.
pub const SHELL: &str = "shell";

/// Is a session with this Status idle, ready for a prompt? `idle`, or Claude Code's
/// [`SHELL`]: the agent waits for the user just the same, a shell running behind it. Checks
/// that would end that shell (closing a job's tmux session, the operator's `/clear`) ask for
/// `idle` itself.
pub fn is_idle(status: &str) -> bool {
    matches!(status, "idle" | SHELL)
}

pub fn claude_home() -> PathBuf {
    dirs::home_dir().unwrap_or_default().join(".claude")
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Backend {
    Iterm,
    Tmux,
    Unknown,
}

impl Backend {
    pub fn label(self) -> &'static str {
        match self {
            Backend::Iterm => "iterm",
            Backend::Tmux => "tmux",
            Backend::Unknown => "?",
        }
    }
}

/// `Deserialize` is here for the fixture mode (`FLEET_FIXTURE`), which feeds
/// the dashboard a canned fleet — used by the golden-buffer tests and as a demo.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Session {
    pub pid: i64,
    pub session_id: Option<String>,
    pub name: Option<String>,
    pub cwd: Option<String>,
    /// The registry's Status, as written: `busy` | `idle` | `waiting` | `unknown`, and from
    /// Claude Code 2.1.308 `shell` — idle, with a background shell still running. Ask
    /// [`Session::is_idle`], not `== "idle"`, whether it is ready for a prompt.
    pub status: String,
    /// Milliseconds since the epoch of the last status update.
    pub updated_at: Option<i64>,
    pub tty: Option<String>,
    pub backend: Backend,
    /// Backend-specific control handle: iTerm session GUID or tmux pane id.
    pub handle: Option<String>,
    /// Terminal tab name: iTerm tab title or tmux window name.
    pub tab: Option<String>,
    /// The tmux *session* the pane lives in — one session per job is the
    /// convention, so this is the terminal identifier worth showing.
    pub tmux_session: Option<String>,
    /// Registry `nameSource`: `"derived"` means Claude made the name up from the
    /// cwd plus a hash, i.e. it carries no information about the work.
    pub name_source: Option<String>,
    /// Registry `waitingFor`: what a `waiting` session is blocked on.
    pub waiting_for: Option<String>,
    pub title: Option<String>,
    /// The LLM-generated title — what the work *is*, not where it lives. Filled
    /// in from `~/.claude/fleet-names.json` and the dashboard's background
    /// titling pass, never by the registry, and deliberately independent of both
    /// `name` and `tab`: it is what the TUI puts on a row's first line.
    pub gen_title: Option<String>,
    /// The one title every view draws — see [`crate::core::title::display_title`].
    /// Stamped by `list` (`None` inside discovery; [`Session::headline`] computes
    /// it live). Output only: views must not edit it, renames go through Claude.
    pub display_title: Option<String>,
    /// The fleet host this session runs on (config `self`, or the name the
    /// caller knows it by). Stamped by `list`; `None` inside discovery.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub host: Option<String>,
    /// Context-window usage from the transcript tail; `null` when unknown.
    pub context: Option<crate::core::context::ContextUsage>,
    /// The session stack it belongs to (`{ id, label }`, see [`crate::core::stack`]);
    /// `null` when none. Stamped by `list` (read-only); `None` inside discovery.
    pub stack: Option<crate::core::stack::StackRef>,
    /// Which coding agent runs the session: `claude` | `codex` | `pi`.
    pub harness: Harness,
    /// The model it runs, when known (Codex / pi: from the transcript or the command
    /// line; Claude: the context entry's model).
    pub model: Option<String>,
    /// The reasoning effort it runs at (`low`, `medium`, `high`, `xhigh`, `max` …), as the
    /// transcript last recorded it (Claude: the context entry's `effort`; Codex: the last
    /// `turn_context`'s); `null` when unknown, and always for pi.
    pub effort: Option<String>,
    /// Claude Code 2.1.301+: tasks running now and tasks started and finished in
    /// the current Turn. `None` when unknown, and always for Codex and pi.
    pub subagents: Option<crate::core::subagents::Counts>,
    /// The `timestamp` of the transcript entry `model` was read from (Claude Code: its last
    /// reply; Codex: its `turn_context`; pi: its last reply or model change), as written;
    /// `None` without one (no model known, or one from the command line).
    pub model_at: Option<String>,
    /// Absolute path of the session's transcript, when known (Codex / pi only — a
    /// Claude transcript is found from cwd + session id).
    pub transcript: Option<String>,
    /// The [job](crate::core::jobs) run that started it (`{ job, run }`); `null` when none.
    pub origin: Option<crate::core::jobs::Origin>,
    /// The agent process's start time, as the identity check reads it (`ps -o lstart` in UTC,
    /// what Claude records as `procStart`), in RFC 3339 UTC (`2026-10-17T13:19:29Z`): with
    /// `pid`, what tells this process from a later one. `null` when it cannot be read.
    pub proc_start: Option<String>,
}

impl Default for Session {
    fn default() -> Self {
        Self {
            pid: 0,
            session_id: None,
            name: None,
            cwd: None,
            status: "unknown".into(),
            updated_at: None,
            tty: None,
            backend: Backend::Unknown,
            handle: None,
            tab: None,
            tmux_session: None,
            name_source: None,
            waiting_for: None,
            title: None,
            gen_title: None,
            display_title: None,
            host: None,
            context: None,
            stack: None,
            harness: Harness::Claude,
            model: None,
            effort: None,
            subagents: None,
            model_at: None,
            transcript: None,
            origin: None,
            proc_start: None,
        }
    }
}

impl Session {
    /// Stable identity key: sessionId if known, else pid. Used to track a session
    /// across polls even as the sorted order shifts.
    pub fn key(&self) -> String {
        self.session_id
            .clone()
            .unwrap_or_else(|| self.pid.to_string())
    }

    /// True when the display name is Claude's cwd+hash fallback rather than
    /// something a human (or an LLM) chose — batch B renames exactly these.
    pub fn is_derived_name(&self) -> bool {
        self.name_source.as_deref() == Some("derived")
    }

    /// Is this session blocked on the user right now?
    pub fn is_waiting(&self) -> bool {
        self.status == "waiting"
    }

    /// Is this session at its prompt, ready for the next one? See [`is_idle`].
    pub fn is_idle(&self) -> bool {
        is_idle(&self.status)
    }

    /// What every view puts on a row: the session's one title,
    /// [`crate::core::title::display_title`] — the chosen Claude name, else the
    /// generated title, else a heuristic, else [`Session::label`].
    pub fn headline(&self) -> String {
        crate::core::title::display_title(self)
    }

    /// Short human label: derived name, else short sessionId, else pid.
    pub fn label(&self) -> String {
        self.name
            .clone()
            .or_else(|| {
                self.session_id
                    .as_ref()
                    .map(|s| s.chars().take(8).collect())
            })
            .or_else(|| {
                // A Codex session before its first prompt has neither: `<cwd>-codex`.
                let dir = self
                    .cwd
                    .as_deref()?
                    .trim_end_matches('/')
                    .rsplit('/')
                    .next()?;
                (self.harness != Harness::Claude && !dir.is_empty())
                    .then(|| format!("{dir}-{}", self.harness))
            })
            .unwrap_or_else(|| self.pid.to_string())
    }
}

/// Entrypoints that are not a session anyone supervises: a `claude -p` child, an
/// SDK run, a CI job. None of them has a TUI to focus, send to or rename.
///
/// They have to be excluded by name because they register themselves exactly like
/// the real thing — `kind: "interactive"` included — and one of them is how this
/// crate generates names: `ask_claude` shells out to `claude -p`, which appears in
/// the registry as an `sdk-cli` session in `$TMPDIR` called `t-d1`. Left in, the
/// dashboard's titling pass discovers its own children and titles them, and each
/// title it pays for spawns the next one.
const HEADLESS_ENTRYPOINTS: [&str; 5] = [
    "sdk-cli",
    "sdk-ts",
    "sdk-py",
    "local-agent",
    "github-action",
];

/// Is this registry entry a headless run rather than a session on a terminal?
/// Unknown entrypoints are kept — a new interactive one (`claude-vscode`,
/// `claude-desktop`, `remote`) belongs in the fleet, and missing a real session is
/// the worse failure of the two.
fn is_headless(entrypoint: &str) -> bool {
    HEADLESS_ENTRYPOINTS.contains(&entrypoint) || entrypoint.starts_with("sdk-")
}

#[derive(serde::Deserialize)]
struct Reg {
    version: Option<serde_json::Value>,
    #[serde(rename = "startedAt")]
    started_at: Option<serde_json::Value>,
    pid: Option<i64>,
    #[serde(rename = "sessionId")]
    session_id: Option<String>,
    cwd: Option<String>,
    name: Option<String>,
    status: Option<String>,
    kind: Option<String>,
    #[serde(rename = "updatedAt")]
    updated_at: Option<i64>,
    #[serde(rename = "statusUpdatedAt")]
    status_updated_at: Option<i64>,
    #[serde(rename = "nameSource")]
    name_source: Option<String>,
    #[serde(rename = "waitingFor")]
    waiting_for: Option<String>,
    /// How this session was started — `"cli"` for a terminal, `"sdk-cli"` for a
    /// `claude -p` child. See [`is_headless`].
    entrypoint: Option<String>,
    /// The process's start time as `ps -o lstart` prints it (UTC): what tells this
    /// session's process from a later one that reused its pid.
    #[serde(rename = "procStart")]
    proc_start: Option<String>,
}

/// One tmux pane, as reported by `list-panes -a`.
#[derive(Debug, Clone)]
pub struct Pane {
    /// `session:window.pane` — the addressable target.
    pub target: String,
    /// `%12` — the stable pane id, preferred as a control handle.
    pub pane_id: String,
    pub window_name: String,
    pub session_name: String,
    /// The pane's `@fleet_job_run` option: the job run it belongs to (empty when none).
    pub job_run: String,
}

/// A canned fleet from `FLEET_FIXTURE=<path/to/sessions.json>`. Bypasses the
/// registry entirely so the dashboard can be rendered (and asserted on) without
/// live sessions.
fn fixture() -> Option<Vec<Session>> {
    let path = std::env::var_os("FLEET_FIXTURE")?;
    let text = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(&text).ok()
}

/// True while the fixture is driving discovery — backends must not be poked.
pub fn is_fixture() -> bool {
    std::env::var_os("FLEET_FIXTURE").is_some()
}

pub fn discover() -> Vec<Session> {
    discover_checked().unwrap_or_default()
}

/// [`discover`], but an unreadable registry is an error rather than an empty
/// fleet — for callers (the stale check) that must not mistake "couldn't look"
/// for "nothing there". A registry that doesn't exist yet is an empty fleet.
pub fn discover_checked() -> Result<Vec<Session>, String> {
    if let Some(rows) = fixture() {
        return Ok(rows);
    }
    let dir = claude_home().join("sessions");
    let panes = tmux_panes();
    let mut out = Vec::new();

    let entries = match std::fs::read_dir(&dir) {
        Ok(e) => e,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(out),
        Err(e) => return Err(format!("cannot read {}: {e}", dir.display())),
    };
    for entry in entries.flatten() {
        let Some(reg) = read_reg(&entry.path()) else {
            continue;
        };
        let Some(pid) = reg.pid else { continue };
        // ps returns non-zero for a dead pid, so this doubles as a liveness check.
        let Some(tty) = tty_of(pid) else { continue };
        let (backend, handle) = backend_of(pid, Some(tty.as_str()), &panes);
        out.push(claude_session(reg, pid, tty, backend, handle, &panes, true));
    }
    crate::core::context::apply_native_1m(out.iter_mut().filter_map(|s| s.context.as_mut()));
    out.extend(discover_harnesses(&panes, None));
    stamp_proc_starts(&mut out);
    out.sort_by_key(|s| std::cmp::Reverse(s.updated_at.unwrap_or(0)));
    Ok(out)
}

/// One registry entry, when the file is one and it is a session anybody supervises.
fn read_reg(path: &std::path::Path) -> Option<Reg> {
    if path.extension().and_then(|s| s.to_str()) != Some("json") {
        return None;
    }
    let reg: Reg = serde_json::from_str(&std::fs::read_to_string(path).ok()?).ok()?;
    // Interactive sessions only (skip print/headless runs). `kind` alone does
    // not say it: a `claude -p` child registers as `interactive` too, and its
    // entrypoint is the only thing that gives it away.
    if reg.kind.as_deref().is_some_and(|k| k != "interactive") {
        return None;
    }
    if reg.entrypoint.as_deref().is_some_and(is_headless) {
        return None;
    }
    Some(reg)
}

/// A live Claude session from its registry entry and where its process runs. `enrich`
/// reads its transcript for the title and context usage — the costly part, which a lookup
/// of one session by an exact target skips.
fn claude_session(
    reg: Reg,
    pid: i64,
    tty: String,
    backend: Backend,
    handle: Option<String>,
    panes: &HashMap<String, Pane>,
    enrich: bool,
) -> Session {
    // tmux tab (window name) is free from the panes query; iTerm tab titles are
    // filled lazily by enrich_iterm_tabs (one AppleScript call) only for display.
    let pane = if backend == Backend::Tmux {
        panes.get(&format!("/dev/{tty}"))
    } else {
        None
    };
    let tab = pane
        .map(|p| p.window_name.clone())
        .filter(|w| !w.is_empty());
    let tmux_session = pane
        .map(|p| p.session_name.clone())
        .filter(|w| !w.is_empty());
    let origin = pane.and_then(|p| crate::core::jobs::Origin::parse(&p.job_run));
    let title = match (enrich, reg.session_id.as_deref(), reg.cwd.as_deref()) {
        (true, Some(sid), Some(cwd)) => title_for(cwd, sid),
        _ => None,
    };
    let (last, subagents) = if enrich {
        claude_details(&claude_home(), &reg)
    } else {
        (None, None)
    };
    let (context, effort, model_at) = match last {
        Some(l) => (Some(l.context), l.effort, l.at),
        None => (None, None, None),
    };
    Session {
        pid,
        status: reg.status.unwrap_or_else(|| "unknown".into()),
        updated_at: reg.status_updated_at.or(reg.updated_at),
        title,
        tty: Some(tty),
        backend,
        handle,
        tab,
        tmux_session,
        name_source: reg.name_source,
        waiting_for: reg.waiting_for,
        session_id: reg.session_id,
        name: reg.name,
        cwd: reg.cwd,
        gen_title: None,
        display_title: None,
        host: None,
        model_at: model_at.filter(|_| context.as_ref().is_some_and(|c| c.model.is_some())),
        model: context.as_ref().and_then(|c| c.model.clone()),
        effort,
        subagents,
        context,
        stack: None,
        harness: Harness::Claude,
        transcript: None,
        origin,
        proc_start: None,
    }
}

/// Sidecars decide whether the shared transcript-tail read needs a Turn start.
fn claude_details(
    home: &std::path::Path,
    reg: &Reg,
) -> (
    Option<crate::core::context::LastReply>,
    Option<crate::core::subagents::Counts>,
) {
    let Some(path) = reg
        .session_id
        .as_deref()
        .and_then(|sid| crate::core::context::locate_transcript(home, reg.cwd.as_deref(), sid))
    else {
        return (None, None);
    };
    let scan = crate::core::subagents::scan(
        &path,
        reg.version.as_ref().and_then(serde_json::Value::as_str),
        reg.started_at.as_ref().and_then(serde_json::Value::as_i64),
    );
    let search = scan.as_ref().and_then(|s| s.search());
    let (last, turn, lifecycle) =
        crate::core::context::for_transcript(&path, reg.cwd.as_deref(), search.as_ref());
    (last, scan.map(|s| s.counts(turn.as_deref(), &lifecycle)))
}

/// Live Codex and pi sessions (see [`crate::core::harness`]): found with `ps`, matched
/// to their transcripts, placed in their terminal like Claude's.
///
/// `only`: build rows just for the process an exact target names (the others are still
/// listed and paired with transcripts — that pairing needs every process in a cwd — but
/// never inspected, read for state or titled).
fn discover_harnesses(panes: &HashMap<String, Pane>, only: Option<&Exact>) -> Vec<Session> {
    if !harness::codex_home().is_dir() && !harness::pi_home().is_dir() {
        return Vec::new();
    }
    let procs = harness::processes();
    let mut out = Vec::new();
    for (h, root) in [
        (Harness::Codex, harness::codex_home()),
        (Harness::Pi, harness::pi_home()),
    ] {
        // No state dir under this `$HOME`: the harness never ran as this user (and a
        // test's temp home never sees the machine's live sessions).
        if !root.is_dir() {
            continue;
        }
        let mine: Vec<&harness::Proc> = procs.iter().filter(|p| p.harness == h).collect();
        if mine.is_empty() {
            continue;
        }
        let ts = harness::transcripts(h, &root, &mine);
        let assigned = harness::assign(&mine, &ts);
        let names = if h == Harness::Codex {
            harness::codex_names(&root)
        } else {
            HashMap::new()
        };
        for p in mine {
            let t = assigned.get(&p.pid);
            let named = match only {
                None => true,
                Some(Exact::Pid(pid)) => p.pid == *pid,
                Some(Exact::Id(id)) => t.is_some_and(|t| t.session_id.eq_ignore_ascii_case(id)),
            };
            if !named {
                continue;
            }
            let mut s = harness_session(p, t, panes);
            if let Some(n) = s.session_id.as_ref().and_then(|id| names.get(id)) {
                // Codex titles every thread itself after its first turn.
                s.name = Some(n.clone());
                s.name_source = Some("codex".into());
            }
            out.push(s);
        }
    }
    out
}

fn harness_session(
    p: &harness::Proc,
    t: Option<&harness::TranscriptRef>,
    panes: &HashMap<String, Pane>,
) -> Session {
    let (backend, handle) = backend_of(p.pid, Some(p.tty.as_str()), panes);
    let pane = (backend == Backend::Tmux)
        .then(|| panes.get(&format!("/dev/{}", p.tty)))
        .flatten();
    let mut state = t
        .map(|t| harness::state(p.harness, &t.path))
        .unwrap_or_else(|| harness::State {
            status: "idle".into(),
            ..Default::default()
        });
    if p.harness == Harness::Codex && state.status == "busy" {
        codex_prompt_from_screen(&mut state, handle.as_deref().filter(|_| pane.is_some()));
    }
    Session {
        pid: p.pid,
        name_source: state.name.is_some().then(|| "user".to_string()),
        name: state.name,
        session_id: t.map(|t| t.session_id.clone()),
        cwd: t.map(|t| t.cwd.clone()).or_else(|| p.cwd.clone()),
        status: state.status,
        waiting_for: state.waiting_for,
        updated_at: Some(t.map(|t| t.mtime_ms).unwrap_or(p.start_ms)),
        tty: Some(p.tty.clone()),
        backend,
        handle,
        tab: pane
            .map(|p| p.window_name.clone())
            .filter(|w| !w.is_empty()),
        tmux_session: pane
            .map(|p| p.session_name.clone())
            .filter(|w| !w.is_empty()),
        origin: pane.and_then(|p| crate::core::jobs::Origin::parse(&p.job_run)),
        title: t.and_then(|t| harness::first_prompt(p.harness, &t.path)),
        model_at: state.model_at.filter(|_| state.model.is_some()),
        model: state.model.or_else(|| p.model_arg()),
        effort: state.effort,
        context: state.context,
        harness: p.harness,
        transcript: t.map(|t| t.path.display().to_string()),
        ..Default::default()
    }
}

/// A busy Codex session may be sitting on an approval prompt that its transcript does not
/// record (Codex 0.162). In a tmux pane the screen says so: it decides, either way. Elsewhere
/// (iTerm, a pane that would not answer) a pending escalated command is the best guess.
fn codex_prompt_from_screen(state: &mut harness::State, tmux_pane: Option<&str>) {
    let screen = tmux_pane.and_then(crate::core::backend::tmux_screen);
    let waits_for = match &screen {
        Some(s) => harness::codex_screen_waits_for(s),
        None => state.escalation_pending.then_some("exec approval"),
    };
    if let Some(w) = waits_for {
        state.status = "waiting".into();
        state.waiting_for = Some(w.into());
    }
}

/// Resolve a target to exactly one session.
///
/// A target is a pid, a sessionId prefix, a session name, a generated title — or
/// any unambiguous fragment of the last two, down to a fuzzy `flt` for
/// `fleet-llm-title`. See [`resolve_in`] for how loose is too loose.
///
/// `pid:<n>` and `id:<session_id>` are the exact forms: they match only that pid or
/// that full session id, and fail rather than fall through. A bare pid does fall
/// through — so a pid that just exited can match a title with those digits in it;
/// callers that type into the result (UIs, scripts) should use the exact forms.
pub fn resolve(target: &str) -> Result<Session, String> {
    resolve_target(target).map_err(|e| e.to_string())
}

/// [`resolve`] with the failure kept typed, for callers that report it as JSON.
pub fn resolve_target(target: &str) -> Result<Session, TargetError> {
    if let Some(exact) = exact_target(target)
        && !is_fixture()
        && let Some(found) = find_exact(target.trim(), &exact?)
    {
        return found;
    }
    let mut rows = discover();
    // Every fleet view prints the *title* as a session's identity, so a title has
    // to resolve: otherwise `peek <the thing list just showed me>` fails for every
    // titled session, which is all of them.
    crate::core::naming::stamp_titles(&mut rows);
    resolve_target_in(rows, target)
}

/// Why a target did not resolve to exactly one session.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum TargetError {
    /// Nothing matched (or, for `pid:` / `id:`, nothing matched exactly).
    #[error("{0}")]
    NotFound(String),
    /// More than one session matched on the same rung.
    #[error("{0}")]
    Ambiguous(String),
    /// An explicit form with a malformed value (`pid:abc`, `id:`), or no target at all.
    #[error("{0}")]
    Invalid(String),
}

impl TargetError {
    /// The stable machine-readable kind a `--json` failure carries.
    pub fn kind(&self) -> &'static str {
        match self {
            TargetError::NotFound(_) => "target_not_found",
            TargetError::Ambiguous(_) => "target_ambiguous",
            TargetError::Invalid(_) => "target_invalid",
        }
    }
}

/// An explicit target: matched exactly, never by prefix, substring or fuzzy title.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Exact {
    /// `pid:<n>` — the pid of a listed session.
    Pid(i64),
    /// `id:<session_id>` — the full session id (case-insensitive).
    Id(String),
}

/// Parse `pid:<n>` / `id:<session_id>`. `None` for a bare target (it keeps the ladder);
/// `Some(Err)` for an explicit form whose value is malformed.
pub fn exact_target(target: &str) -> Option<Result<Exact, TargetError>> {
    let t = target.trim();
    let (prefix, value) = t.split_once(':')?;
    let value = value.trim();
    match prefix.trim().to_ascii_lowercase().as_str() {
        "pid" => Some(
            value
                .parse::<i64>()
                .ok()
                .filter(|p| *p > 0)
                .map(Exact::Pid)
                .ok_or_else(|| {
                    TargetError::Invalid(format!(
                        "\"{target}\": pid: takes a process id (digits only)"
                    ))
                }),
        ),
        "id" => Some(if value.is_empty() {
            Err(TargetError::Invalid(format!(
                "\"{target}\": id: takes a full session id"
            )))
        } else {
            Ok(Exact::Id(value.to_string()))
        }),
        _ => None,
    }
}

/// The one session an [`Exact`] target names. Never falls through to a looser match:
/// a pid that is gone is an error even when it also reads as a fragment of a title.
fn resolve_exact(rows: Vec<Session>, target: &str, exact: &Exact) -> Result<Session, TargetError> {
    let is = |s: &Session| match exact {
        Exact::Pid(p) => s.pid == *p,
        Exact::Id(id) => s
            .session_id
            .as_deref()
            .is_some_and(|sid| sid.eq_ignore_ascii_case(id)),
    };
    let mut hits: Vec<Session> = rows.into_iter().filter(is).collect();
    match hits.len() {
        1 => Ok(hits.remove(0)),
        0 => Err(TargetError::NotFound(match exact {
            Exact::Pid(p) => format!("no live session has pid {p} (\"{target}\" matches exactly)"),
            Exact::Id(id) => {
                format!("no live session has session id {id} (\"{target}\" matches exactly)")
            }
        })),
        n => Err(TargetError::Ambiguous(format!(
            "\"{target}\" matches {n} sessions: {} — be more specific",
            hits.iter()
                .map(|s| format!("{} (pid {})", s.headline(), s.pid))
                .collect::<Vec<_>>()
                .join(", ")
        ))),
    }
}

/// The lookup behind an exact target, without discovering the whole fleet: the registry
/// entries naming that pid / session id (`<pid>.json` for `pid:`; for `id:`, every entry is
/// parsed to find it — files only, no process), one `ps` over their pids (liveness, tty,
/// start time, environment), the tmux pane list only when one runs in tmux, and a bounded
/// read of the chosen session's transcript head for its title — no other transcript, no
/// context usage. A UI polling one session (`peek --json id:…` every second) costs a
/// handful of processes instead of two per live session.
///
/// Identity: the process must still be the one the entry registered — when the entry
/// records `procStart`, the live pid's start time has to match it, so a pid that a newer
/// process reused is not taken for the session.
///
/// No live Claude session in the registry (none, or only stale entries): a Codex / pi
/// session (they have none) is looked for among the harness processes alone — one `ps`, one `lsof`, and only the named process
/// inspected and its transcript read.
fn find_exact(target: &str, exact: &Exact) -> Option<Result<Session, TargetError>> {
    let mut rows = exact_rows(exact, None);
    crate::core::naming::stamp_titles(&mut rows);
    Some(resolve_exact(rows, target, exact).map(|mut s| {
        // The one row picked gets its title (a bounded read of its transcript's head): it
        // is what `name` works from, and what `rename --json`'s `from` and the views show.
        if s.harness == Harness::Claude
            && let (None, Some(sid), Some(cwd)) =
                (&s.title, s.session_id.as_deref(), s.cwd.as_deref())
        {
            s.title = title_for(cwd, sid);
        }
        s
    }))
}

/// The live sessions of `harness` whose session id is `id` (whole, case-insensitive), by the
/// lookup behind an `id:` target ([`find_exact`]) narrowed to that harness: another
/// harness's entry with the same id, live or stale, neither stands in for this one nor
/// hides it. Claude's come from its registry (a stale entry is no session), Codex's and pi's
/// from their processes. Under `FLEET_FIXTURE`, the fixture's rows.
pub fn find_live_id(harness: Harness, id: &str) -> Vec<Session> {
    let exact = Exact::Id(id.to_string());
    let rows = if is_fixture() {
        discover()
    } else {
        exact_rows(&exact, Some(harness))
    };
    rows.into_iter()
        .filter(|s| {
            s.harness == harness
                && s.session_id
                    .as_deref()
                    .is_some_and(|sid| sid.eq_ignore_ascii_case(id))
        })
        .collect()
}

/// The live rows an exact target could name, untitled. `only`: of that harness alone; else
/// Claude's registry first and, when no live Claude session is it, Codex's and pi's
/// processes (a stale Claude entry does not hide a live Codex / pi session).
fn exact_rows(exact: &Exact, only: Option<Harness>) -> Vec<Session> {
    let rows = if only.is_none_or(|h| h == Harness::Claude) {
        claude_exact_rows(exact)
    } else {
        Vec::new()
    };
    if !rows.is_empty() || only == Some(Harness::Claude) {
        return rows;
    }
    // Codex / pi sessions have no registry: only the harness processes are listed, and
    // only the named one is inspected and read.
    if !harness::codex_home().is_dir() && !harness::pi_home().is_dir() {
        return Vec::new();
    }
    let mut rows = discover_harnesses(&tmux_panes(), Some(exact));
    if let Some(h) = only {
        rows.retain(|s| s.harness == h);
    }
    rows
}

/// The live Claude sessions an exact target names, from their registry entries alone.
fn claude_exact_rows(exact: &Exact) -> Vec<Session> {
    let dir = claude_home().join("sessions");
    let mut regs: Vec<Reg> = match exact {
        Exact::Pid(p) => read_reg(&dir.join(format!("{p}.json")))
            .filter(|r| r.pid == Some(*p))
            .into_iter()
            .collect(),
        Exact::Id(_) => Vec::new(),
    };
    if regs.is_empty() {
        // `<pid>.json` is Claude's naming; anything else is found by reading the entries.
        regs = std::fs::read_dir(&dir)
            .into_iter()
            .flatten()
            .flatten()
            .filter_map(|e| read_reg(&e.path()))
            .filter(|r| match exact {
                Exact::Pid(p) => r.pid == Some(*p),
                Exact::Id(id) => r
                    .session_id
                    .as_deref()
                    .is_some_and(|sid| sid.eq_ignore_ascii_case(id)),
            })
            .collect();
    }
    if regs.is_empty() {
        return Vec::new();
    }
    let pids: Vec<i64> = regs.iter().filter_map(|r| r.pid).collect();
    let procs = inspect(&pids);
    let mut live: Vec<(Reg, i64, &Inspected)> = Vec::new();
    for reg in regs {
        let Some(pid) = reg.pid else { continue };
        let Some(p) = procs.get(&pid) else { continue };
        if reg
            .proc_start
            .as_deref()
            .is_some_and(|want| !p.started_as(want))
        {
            continue;
        }
        live.push((reg, pid, p));
    }
    let needs_panes = live
        .iter()
        .any(|(_, _, p)| !p.env.contains("ITERM_SESSION_ID=") || p.env.contains("TMUX="));
    let panes = if needs_panes {
        tmux_panes()
    } else {
        HashMap::new()
    };
    live.into_iter()
        .map(|(reg, pid, p)| {
            let (backend, handle) = backend_from_env(&p.env, Some(p.tty.as_str()), &panes);
            Session {
                // The start the identity check just compared with `procStart`.
                proc_start: start_rfc3339(&p.start),
                ..claude_session(reg, pid, p.tty.clone(), backend, handle, &panes, false)
            }
        })
        .collect()
}

/// One live process as a single `ps` reports it.
#[derive(Debug)]
struct Inspected {
    tty: String,
    /// `ps -o lstart` in UTC (`LC_ALL=C TZ=UTC`, as Claude records `procStart`), words
    /// single-spaced.
    start: String,
    /// The command line followed by the environment (`ps e`).
    env: String,
}

impl Inspected {
    /// Is `proc_start` (the registry's `procStart`) this process's start? Claude writes it
    /// with `LC_ALL=C TZ=UTC ps -o lstart=`, which is how [`inspect`] reads it too: compared
    /// as UTC times, else (either side unparseable) as single-spaced strings — so an entry
    /// that cannot be checked fails closed rather than vouching for whatever has the pid.
    fn started_as(&self, proc_start: &str) -> bool {
        match (parse_lstart(&self.start), parse_lstart(proc_start)) {
            (Some(have), Some(want)) => have == want,
            _ => self.start == single_spaced(proc_start),
        }
    }
}

/// Each row's [`Session::proc_start`], from one `ps` over their pids — the same reading
/// ([`inspect`]'s `lstart`, in UTC) the identity check compares with `procStart`. An exact
/// lookup has it from that check instead (a Claude session), or leaves it `None` (Codex /
/// pi: [`process_start`] reads it when needed).
fn stamp_proc_starts(rows: &mut [Session]) {
    let pids: Vec<i64> = rows.iter().map(|r| r.pid).collect();
    let starts = proc_starts(&pids);
    for r in rows {
        r.proc_start = starts.get(&r.pid).and_then(|s| start_rfc3339(s));
    }
}

/// `ps -o lstart` (UTC) of each live pid of `pids`.
fn proc_starts(pids: &[i64]) -> HashMap<i64, String> {
    if pids.is_empty() {
        return HashMap::new();
    }
    let list = pids
        .iter()
        .map(i64::to_string)
        .collect::<Vec<_>>()
        .join(",");
    let Ok(out) = Command::new("ps")
        .env("TZ", "UTC")
        .env("LC_ALL", "C")
        .args(["-o", "pid=,lstart=", "-p", &list])
        .output()
    else {
        return HashMap::new();
    };
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(|l| {
            let (pid, start) = l.trim().split_once(' ')?;
            Some((pid.parse().ok()?, single_spaced(start)))
        })
        .collect()
}

/// The live start time of process `pid`, as [`Session::proc_start`] carries it (RFC 3339
/// UTC); `None` when there is no such process or its start cannot be read.
pub fn process_start(pid: i64) -> Option<String> {
    proc_starts(&[pid]).get(&pid).and_then(|s| start_rfc3339(s))
}

/// One process, as a [`processes`] snapshot read it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Proc {
    pub pid: i64,
    pub ppid: i64,
    /// Its process group.
    pub pgid: i64,
    /// The foreground process group of its terminal: the one what is typed there reaches.
    pub tpgid: i64,
    /// `ps` state: `S+`, `T` (stopped, as Ctrl-Z leaves it), `Z` (a zombie), ….
    pub stat: String,
    /// Its terminal as `ps` names it (`ttys012`, `pts/3`); `None` when it has none.
    pub tty: Option<String>,
    /// Its start time, RFC 3339 UTC as [`Session::proc_start`]; `None` when unreadable.
    pub start: Option<String>,
    pub command: String,
}

impl Proc {
    /// Exited but not yet reaped.
    pub fn zombie(&self) -> bool {
        self.stat.starts_with('Z')
    }

    /// Stopped (Ctrl-Z, `SIGSTOP`) or traced.
    pub fn stopped(&self) -> bool {
        self.stat.contains(['T', 't'])
    }

    /// Running in the foreground of its terminal: in the terminal's foreground process group
    /// and not stopped. What is typed on that terminal reaches only such a process.
    pub fn in_front(&self) -> bool {
        self.tty.is_some() && self.pgid == self.tpgid && !self.stopped() && !self.zombie()
    }
}

/// Every process, from one `ps`: a process's pid, start time, terminal and foreground state
/// are read together, so they never mix two processes that held the same pid in turn.
pub fn processes() -> std::io::Result<Vec<Proc>> {
    read_procs(&["-A"])
}

/// The processes of `pids` that still run, from one `ps` each read whole as [`processes`]
/// reads them — only those, not every process. A pid that is gone is just not there.
pub fn processes_of(pids: &[i64]) -> std::io::Result<Vec<Proc>> {
    if pids.is_empty() {
        return Ok(Vec::new());
    }
    let list = pids
        .iter()
        .map(i64::to_string)
        .collect::<Vec<_>>()
        .join(",");
    read_procs(&["-p", &list])
}

/// `ps <select> -o …` → [`Proc`]s. `ps -p` exits 1, silently, when none of its pids runs:
/// that is no process, not a failure; anything it says on stderr is.
fn read_procs(select: &[&str]) -> std::io::Result<Vec<Proc>> {
    let out = Command::new("ps")
        .env("TZ", "UTC")
        .env("LC_ALL", "C")
        .args(select)
        .args(["-o", "pid=,ppid=,pgid=,tpgid=,stat=,tty=,lstart=,comm="])
        .output()?;
    let stderr = String::from_utf8_lossy(&out.stderr);
    if !out.status.success() && (select[0] == "-A" || !stderr.trim().is_empty()) {
        return Err(std::io::Error::other(format!(
            "ps exited {}: {}",
            out.status,
            stderr.trim()
        )));
    }
    Ok(String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(parse_proc)
        .collect())
}

/// `pid ppid pgid tpgid stat tty Www Mmm dd hh:mm:ss yyyy command…` → a [`Proc`].
fn parse_proc(line: &str) -> Option<Proc> {
    let w: Vec<&str> = line.split_whitespace().collect();
    if w.len() < 11 {
        return None;
    }
    let num = |i: usize| w[i].parse::<i64>().ok();
    Some(Proc {
        pid: num(0)?,
        ppid: num(1)?,
        pgid: num(2)?,
        tpgid: num(3)?,
        stat: w[4].to_string(),
        tty: (!matches!(w[5], "??" | "?" | "-")).then(|| w[5].to_string()),
        start: start_rfc3339(&w[6..11].join(" ")),
        command: w[11..].join(" "),
    })
}

/// Which of `pids` read terminal `tty` (as `ps` names it: `ttys012`, `pts/3`) — have it as
/// their standard input. `/proc` where there is one, else `lsof`.
///
/// Fails closed: an answer it cannot be sure of is an error, never "nobody". `/proc`: a
/// link that cannot be read for any reason but the process (or its fd 0) being gone. `lsof`:
/// one that cannot be run, runs longer than [`LSOF_DEADLINE`] (it is killed), is killed, or
/// exits non-zero while a pid it said nothing of still runs.
pub fn reading_tty(pids: &[i64], tty: &str) -> std::io::Result<Vec<i64>> {
    if pids.is_empty() {
        return Ok(Vec::new());
    }
    let dev = format!("/dev/{tty}");
    if std::path::Path::new("/proc/self/fd").is_dir() {
        let mut hits = Vec::new();
        for &p in pids {
            match std::fs::read_link(format!("/proc/{p}/fd/0")) {
                Ok(l) if l.as_os_str() == dev.as_str() => hits.push(p),
                Ok(_) => {}
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => {
                    return Err(std::io::Error::other(format!("/proc/{p}/fd/0: {e}")));
                }
            }
        }
        return Ok(hits);
    }
    let list = pids
        .iter()
        .map(i64::to_string)
        .collect::<Vec<_>>()
        .join(",");
    // `-a`: fd 0 of those pids. `-b`: no kernel call that can block. Its exit status is 1
    // when any of them is gone (or cannot be looked at); a process that runs with fd 0 closed
    // gets no record, with exit status 0.
    let out = output_within(
        Command::new("lsof").args([
            "-b", "-n", "-P", "-w", "-a", "-d", "0", "-F", "pn", "-p", &list,
        ]),
        LSOF_DEADLINE,
    )
    .map_err(|e| std::io::Error::new(e.kind(), format!("lsof: {e}")))?;
    let stdout = String::from_utf8_lossy(&out.stdout);
    let (listed, hits) = stdin_on(&stdout, &dev);
    if !out.status.success() {
        let unlisted: Vec<i64> = pids
            .iter()
            .copied()
            .filter(|p| !listed.contains(p))
            .collect();
        let running = processes_of(&unlisted)?;
        if let Some(p) = running.iter().find(|p| !p.zombie()) {
            return Err(std::io::Error::other(format!(
                "lsof exited {} and said nothing of pid {}, which still runs",
                out.status, p.pid
            )));
        }
    }
    Ok(hits)
}

/// How long `lsof` gets to say who reads a terminal, before it is killed and the answer is
/// an error.
pub const LSOF_DEADLINE: std::time::Duration = std::time::Duration::from_secs(3);

/// `cmd`'s output (stdin null, stdout captured, stderr dropped), or `TimedOut` once it has
/// run for `deadline` — it is killed and reaped then.
fn output_within(
    cmd: &mut Command,
    deadline: std::time::Duration,
) -> std::io::Result<std::process::Output> {
    use std::io::Read;
    use std::process::Stdio;
    let mut child = cmd
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()?;
    let mut pipe = child.stdout.take();
    let reader = std::thread::spawn(move || {
        let mut buf = Vec::new();
        if let Some(p) = pipe.as_mut() {
            let _ = p.read_to_end(&mut buf);
        }
        buf
    });
    let end = std::time::Instant::now() + deadline;
    let status = loop {
        if let Some(st) = child.try_wait()? {
            break st;
        }
        if std::time::Instant::now() >= end {
            let _ = child.kill();
            let _ = child.wait();
            return Err(std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                format!("no answer within {}s", deadline.as_secs()),
            ));
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    };
    let stdout = reader.join().unwrap_or_default();
    Ok(std::process::Output {
        status,
        stdout,
        stderr: Vec::new(),
    })
}

/// `lsof -F pn` output → (every pid it has a record of, the pids whose file is `dev`).
fn stdin_on(lsof: &str, dev: &str) -> (Vec<i64>, Vec<i64>) {
    let mut pid = None;
    let mut listed = Vec::new();
    let mut hits = Vec::new();
    for l in lsof.lines() {
        if let Some(p) = l.strip_prefix('p') {
            pid = p.parse().ok();
            if let Some(p) = pid
                && !listed.contains(&p)
            {
                listed.push(p);
            }
        } else if let (Some(n), Some(p)) = (l.strip_prefix('n'), pid)
            && n == dev
            && !hits.contains(&p)
        {
            hits.push(p);
        }
    }
    (listed, hits)
}

/// `ps -o lstart` (UTC) → RFC 3339 UTC, to the second.
pub fn start_rfc3339(lstart: &str) -> Option<String> {
    parse_lstart(lstart).map(|t| {
        t.and_utc()
            .to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
    })
}

/// Are two start times — RFC 3339 (as [`Session::proc_start`] and `close --expect-start`
/// carry them) or `ps -o lstart` in UTC (as `procStart`) — the same instant, to the second?
/// Either unreadable: no (an identity that cannot be checked fails closed).
pub fn same_start(a: &str, b: &str) -> bool {
    let at = |s: &str| {
        chrono::DateTime::parse_from_rfc3339(s.trim())
            .ok()
            .map(|t| t.naive_utc())
            .or_else(|| parse_lstart(s))
            .map(|t| t.and_utc().timestamp())
    };
    matches!((at(a), at(b)), (Some(x), Some(y)) if x == y)
}

fn single_spaced(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn parse_lstart(s: &str) -> Option<chrono::NaiveDateTime> {
    chrono::NaiveDateTime::parse_from_str(&single_spaced(s), "%a %b %e %H:%M:%S %Y").ok()
}

/// One `ps` for every pid: tty, start time and environment of each that is alive and on a
/// terminal. Dead pids, and ones with no tty, are simply absent.
fn inspect(pids: &[i64]) -> HashMap<i64, Inspected> {
    if pids.is_empty() {
        return HashMap::new();
    }
    let list = pids
        .iter()
        .map(i64::to_string)
        .collect::<Vec<_>>()
        .join(",");
    let Ok(out) = Command::new("ps")
        .env("TZ", "UTC")
        .env("LC_ALL", "C")
        .args(["eww", "-o", "pid=,tty=,lstart=,command=", "-p", &list])
        .output()
    else {
        return HashMap::new();
    };
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(parse_inspected)
        .collect()
}

/// `pid tty Www Mmm dd hh:mm:ss yyyy command… ENV=…` → the pid and what we need of it.
fn parse_inspected(line: &str) -> Option<(i64, Inspected)> {
    let words: Vec<&str> = line.split_whitespace().collect();
    if words.len() < 7 {
        return None;
    }
    let pid = words[0].parse().ok()?;
    let tty = words[1];
    if tty.is_empty() || tty == "??" || tty == "?" {
        return None;
    }
    Some((
        pid,
        Inspected {
            tty: tty.to_string(),
            start: words[2..7].join(" "),
            env: words[7..].join(" "),
        },
    ))
}

/// Shortest query allowed to match by subsequence. Two characters spread across a
/// 30-character headline is not a name anyone typed on purpose, and the looser the
/// rung the more a stray hit costs: `send` and `rename` type into a live session.
const MIN_FUZZY: usize = 3;

/// How hard we had to look to match a target. Walked in order, and the **first
/// rung with any hits decides** — so a name typed in full always beats a fuzzy
/// interpretation of it, and looseness is only ever reached for a query that
/// nothing tighter explains.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Rung {
    /// A pid, a whole name, a whole title, or a sessionId prefix.
    Exact,
    /// The headline or name starts with the query — `fleet` for `fleet-llm-title`.
    Prefix,
    /// The query appears somewhere in them — `llm` for `fleet-llm-title`.
    Substring,
    /// The query's characters appear in order — `fltitle`, `flt`, `f-l-t`.
    Subsequence,
}

const RUNGS: [Rung; 4] = [
    Rung::Exact,
    Rung::Prefix,
    Rung::Substring,
    Rung::Subsequence,
];

impl Rung {
    fn matches(self, s: &Session, q: &str) -> bool {
        match self {
            Rung::Exact => {
                s.pid.to_string() == q
                    || s.name.as_deref().is_some_and(|n| n.to_lowercase() == q)
                    || s.gen_title
                        .as_deref()
                        .is_some_and(|t| t.to_lowercase() == q)
                    || s.session_id
                        .as_deref()
                        .is_some_and(|id| id.to_lowercase().starts_with(q))
            }
            Rung::Prefix => Self::any(s, |h| h.starts_with(q)),
            Rung::Substring => Self::any(s, |h| h.contains(q)),
            Rung::Subsequence => {
                q.chars().count() >= MIN_FUZZY && Self::any(s, |h| is_subsequence(q, &h))
            }
        }
    }

    /// The strings a loose target may match on: what the fleet views *print* as
    /// this session's identity, plus the Claude session name behind it. The name
    /// stays in because it is still what `rename` reports and what a user who
    /// knows their fleet by `app-9d` will type.
    fn any(s: &Session, f: impl Fn(String) -> bool) -> bool {
        let headline = s.headline().to_lowercase();
        let name = s.name.as_deref().unwrap_or_default().to_lowercase();
        f(headline) || (!name.is_empty() && f(name))
    }
}

/// Do `q`'s characters appear in `haystack`, in order but not necessarily
/// adjacent? This is the whole of the fuzzy matching: it lets `fltitle` and
/// `f-l-t` find `fleet-llm-title` without scoring anything, which matters because
/// nothing here ever picks a "best" match — see [`resolve_in`].
fn is_subsequence(q: &str, haystack: &str) -> bool {
    let mut chars = haystack.chars();
    q.chars().all(|c| chars.any(|h| h == c))
}

/// The matching behind [`resolve`], over a row set the caller supplies — split out
/// so the rules can be exercised without a registry or a name cache.
///
/// **One hit or an error, never a best guess.** Each rung of [`RUNGS`] is all-or-
/// nothing: two sessions matching at the same looseness is an ambiguous request,
/// and the answer is the list of candidates rather than whichever scored higher.
/// That is what makes fuzzy matching safe to hand to `send` and `rename`, which
/// type into a live Claude TUI — the cost of resolving to the wrong session there
/// is somebody else's turn, so a coin flip is not an acceptable tie-break.
pub fn resolve_in(rows: Vec<Session>, target: &str) -> Result<Session, String> {
    resolve_target_in(rows, target).map_err(|e| e.to_string())
}

/// [`resolve_in`] with the failure kept typed. `pid:<n>` and `id:<session_id>` match
/// exactly and nothing else (see [`exact_target`]); a bare target walks [`RUNGS`].
pub fn resolve_target_in(rows: Vec<Session>, target: &str) -> Result<Session, TargetError> {
    if let Some(exact) = exact_target(target) {
        return resolve_exact(rows, target.trim(), &exact?);
    }
    let q = target.trim().to_lowercase();
    if q.is_empty() {
        return Err(TargetError::Invalid("no target given".into()));
    }
    for rung in RUNGS {
        let mut hits = rows.iter().filter(|r| rung.matches(r, &q));
        let Some(first) = hits.next() else { continue };
        if hits.next().is_none() {
            return Ok(first.clone());
        }
        let all: Vec<String> = rows
            .iter()
            .filter(|r| rung.matches(r, &q))
            .map(Session::headline)
            .collect();
        return Err(TargetError::Ambiguous(format!(
            "\"{target}\" matches {} sessions: {} — be more specific",
            all.len(),
            all.join(", ")
        )));
    }
    Err(TargetError::NotFound(format!(
        "no live session matches \"{target}\""
    )))
}

/// The Claude session this process was launched from: the nearest ancestor pid
/// that owns a registry entry (fleet runs as claude -> shell -> us). `None`
/// when invoked straight from a terminal.
pub fn origin() -> Option<Session> {
    let mut chain = Vec::new();
    let mut pid = std::process::id() as i64;
    for _ in 0..6 {
        let Some(parent) = ppid_of(pid) else { break };
        if parent <= 1 {
            break;
        }
        chain.push(parent);
        pid = parent;
    }
    let rows = discover();
    chain
        .into_iter()
        .find_map(|p| rows.iter().find(|s| s.pid == p).cloned())
}

// --- process introspection ---------------------------------------------------

fn ppid_of(pid: i64) -> Option<i64> {
    let out = Command::new("ps")
        .args(["-o", "ppid=", "-p", &pid.to_string()])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    String::from_utf8_lossy(&out.stdout).trim().parse().ok()
}

/// Returns the tty of a live process, `None` if the process is dead. A live
/// process with no controlling tty yields `Some("")` collapsed to `None` here,
/// which is fine: such a session can't be controlled anyway.
fn tty_of(pid: i64) -> Option<String> {
    let out = Command::new("ps")
        .args(["-o", "tty=", "-p", &pid.to_string()])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let tty = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if tty.is_empty() || tty == "??" || tty == "?" {
        return None;
    }
    Some(tty)
}

fn env_of(pid: i64) -> String {
    Command::new("ps")
        .args(["eww", "-p", &pid.to_string()])
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).to_string())
        .unwrap_or_default()
}

/// Detect the backend from the process environment (TMUX -> tmux, ITERM_SESSION_ID
/// -> iTerm) and resolve the handle its adapter needs.
fn backend_of(
    pid: i64,
    tty: Option<&str>,
    panes: &HashMap<String, Pane>,
) -> (Backend, Option<String>) {
    backend_from_env(&env_of(pid), tty, panes)
}

/// [`backend_of`] minus the `ps` call. When the environment says nothing (macOS
/// hides it for some processes, e.g. platform binaries or another user's), a tty
/// that belongs to a tmux pane still means tmux: the pane owns that terminal.
fn backend_from_env(
    env: &str,
    tty: Option<&str>,
    panes: &HashMap<String, Pane>,
) -> (Backend, Option<String>) {
    let pane_handle = || {
        tty.and_then(|t| {
            panes.get(&format!("/dev/{t}")).map(|p| {
                if p.pane_id.is_empty() {
                    p.target.clone()
                } else {
                    p.pane_id.clone()
                }
            })
        })
    };
    if env.starts_with("TMUX=") || env.contains(" TMUX=") {
        return (Backend::Tmux, pane_handle());
    }
    if let Some(idx) = env.find("ITERM_SESSION_ID=") {
        let rest = &env[idx + "ITERM_SESSION_ID=".len()..];
        let token = rest.split_whitespace().next().unwrap_or("");
        if let Some((_, guid)) = token.split_once(':') {
            return (Backend::Iterm, Some(guid.to_string()));
        }
    }
    if let Some(handle) = pane_handle() {
        return (Backend::Tmux, Some(handle));
    }
    (Backend::Unknown, None)
}

/// pane_tty -> pane for every tmux pane, empty if tmux isn't running.
fn tmux_panes() -> HashMap<String, Pane> {
    let mut map = HashMap::new();
    let Ok(out) = crate::core::tools::tmux_cmd()
        .args([
            "list-panes",
            "-a",
            "-F",
            "#{pane_tty}\t#{session_name}:#{window_index}.#{pane_index}\t#{pane_id}\t#{window_name}\t#{session_name}\t#{@fleet_job_run}",
        ])
        .output()
    else {
        return map;
    };
    for line in String::from_utf8_lossy(&out.stdout).lines() {
        map.extend(parse_pane(line));
    }
    map
}

/// `pane_tty \t target \t pane_id \t window_name \t session_name \t job_run` -> (tty, pane).
fn parse_pane(line: &str) -> Option<(String, Pane)> {
    let mut parts = line.split('\t');
    let ptty = parts.next()?;
    let target = parts.next()?;
    let pane_id = parts.next()?;
    let window_name = parts.next().unwrap_or("").to_string();
    // session_name is also the prefix of `target`, but parsing it back out would
    // break on the `:` a session name is allowed to contain — ask tmux instead.
    let session_name = parts.next().unwrap_or("").to_string();
    let job_run = parts.next().unwrap_or("").to_string();
    Some((
        ptty.to_string(),
        Pane {
            target: target.to_string(),
            pane_id: pane_id.to_string(),
            window_name,
            session_name,
            job_run,
        },
    ))
}

/// Fill iTerm sessions' `tab` with their tab title via a single AppleScript call.
/// Kept out of `discover()` so one-shot commands (peek/send) don't pay for it.
pub fn enrich_iterm_tabs(rows: &mut [Session]) {
    // FLEET_PATH_ONLY refuses iTerm; tab names are cosmetic, so they are just left out.
    if is_fixture() || crate::core::tools::path_only() {
        return;
    }
    if !rows
        .iter()
        .any(|r| r.backend == Backend::Iterm && r.tab.is_none())
    {
        return;
    }
    // `tab` must be bound OUTSIDE the `tell` block: inside it, iTerm2's dictionary
    // defines a `tab` class that shadows AppleScript's tab-character constant, so
    // `& tab &` concatenates the literal text "tab" and the separator disappears.
    let script = r#"set sep to ASCII character 9
tell application "iTerm2"
  set out to ""
  repeat with w in windows
    repeat with t in tabs of w
      set tt to ""
      try
        set tt to title of t
      end try
      repeat with s in sessions of t
        set out to out & (id of s) & sep & tt & linefeed
      end repeat
    end repeat
  end repeat
  return out
end tell"#;
    let Ok(out) = Command::new("osascript").arg("-e").arg(script).output() else {
        return;
    };
    if !out.status.success() {
        return;
    }
    let mut names = HashMap::new();
    for line in String::from_utf8_lossy(&out.stdout).lines() {
        if let Some((id, name)) = line.split_once('\t') {
            let name = name.trim();
            if !name.is_empty() {
                names.insert(id.to_string(), name.to_string());
            }
        }
    }
    for r in rows.iter_mut() {
        if r.backend == Backend::Iterm
            && let Some(name) = r.handle.as_deref().and_then(|g| names.get(g))
        {
            r.tab = Some(name.clone());
        }
    }
}

// --- transcript title --------------------------------------------------------

pub(crate) fn encode_cwd(cwd: &str) -> String {
    cwd.chars()
        .map(|c| if c == '/' || c == '.' { '-' } else { c })
        .collect()
}

/// How much of a transcript's head [`title_for`] reads at most.
const TITLE_SCAN_BYTES: u64 = 1024 * 1024;

/// Title = the session's first genuine user prompt from its transcript. Streamed and
/// bounded — the first 400 lines within the first [`TITLE_SCAN_BYTES`] — so a long
/// transcript costs no more than a short one.
fn title_for(cwd: &str, session_id: &str) -> Option<String> {
    use std::io::{BufRead, Read};
    let file = claude_home()
        .join("projects")
        .join(encode_cwd(cwd))
        .join(format!("{session_id}.jsonl"));
    let f = std::fs::File::open(&file).ok()?;
    let lines = std::io::BufReader::new(f.take(TITLE_SCAN_BYTES))
        .lines()
        .map_while(Result::ok);
    for line in lines.take(400) {
        let line = line.as_str();
        if !line.contains("\"type\":\"user\"") {
            continue;
        }
        let Ok(v) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        if v["message"]["role"] != "user" {
            continue;
        }
        let content = &v["message"]["content"];
        let t = if let Some(s) = content.as_str() {
            s.to_string()
        } else if let Some(arr) = content.as_array() {
            arr.iter()
                .find(|b| b["type"] == "text")
                .and_then(|b| b["text"].as_str())
                .unwrap_or("")
                .to_string()
        } else {
            String::new()
        };
        let t = t.trim();
        if !t.is_empty() && !t.starts_with('<') && !t.starts_with('/') {
            return Some(t.to_string());
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_versions_gate_subagents_and_the_shared_tail_defines_the_turn() {
        let home = tempfile::tempdir().unwrap();
        let project = home.path().join("projects/-work-project");
        let agents = project.join("session/subagents");
        std::fs::create_dir_all(&agents).unwrap();
        std::fs::write(
            agents.join("agent-finished.meta.json"),
            r#"{"finished":true}"#,
        )
        .unwrap();
        std::fs::write(
            agents.join("agent-finished.jsonl"),
            "{\"timestamp\":\"2026-01-01T00:01:02Z\"}\n",
        )
        .unwrap();
        let transcript = project.join("session.jsonl");
        let fixtures =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../testdata/subagents");
        for version in ["2.1.300", "2.1.301"] {
            let reg: Reg = serde_json::from_str(
                &std::fs::read_to_string(fixtures.join(format!("registry-{version}.json")))
                    .unwrap(),
            )
            .unwrap();
            for (file, expected) in [
                ("typed.jsonl", Some(1)),
                ("mid-turn.jsonl", Some(0)),
                ("followed-by-noise.jsonl", Some(1)),
                ("no-prompt.jsonl", None),
            ] {
                std::fs::copy(fixtures.join(file), &transcript).unwrap();
                let (last, count) = claude_details(home.path(), &reg);
                assert_eq!(last.unwrap().context.used, 10);
                if version == "2.1.300" {
                    assert!(count.is_none());
                } else {
                    assert_eq!(count.unwrap().finished, expected, "{file}");
                }
            }
        }
        // Unknown registry field shapes must not hide an otherwise usable Session.
        let reg: Reg = serde_json::from_value(serde_json::json!({
            "sessionId": "session", "cwd": "/work/project", "version": 123, "startedAt": "unknown"
        }))
        .unwrap();
        assert!(claude_details(home.path(), &reg).1.is_none());
    }

    #[test]
    fn list_rows_carry_null_for_unknown_and_unsupported_harnesses() {
        assert_eq!(
            serde_json::to_value(Session::default()).unwrap()["subagents"],
            serde_json::Value::Null
        );
        let root =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../testdata/subagents/codex");
        for agent in [Harness::Codex, Harness::Pi] {
            let proc = harness::Proc {
                pid: 42,
                tty: "ttys001".into(),
                harness: agent,
                start_ms: 0,
                argv: vec![agent.id().into()],
                cwd: Some("/work/project".into()),
            };
            // Even child rollouts in different states remain outside the count's scope.
            for name in [
                None,
                Some("parent"),
                Some("running"),
                Some("finished"),
                Some("prior"),
            ] {
                let transcript = name.map(|name| harness::TranscriptRef {
                    path: root.join(format!("{name}.jsonl")),
                    session_id: name.into(),
                    cwd: "/work/project".into(),
                    created_ms: 0,
                    mtime_ms: 0,
                });
                let s = harness_session(&proc, transcript.as_ref(), &HashMap::new());
                let row = serde_json::to_value(&s).unwrap();
                assert!(row.as_object().unwrap().contains_key("subagents"));
                assert!(row["subagents"].is_null());
            }
        }
    }

    #[test]
    fn lstart_becomes_rfc3339_utc() {
        assert_eq!(
            start_rfc3339("Sat Oct 17 13:19:29 2026").as_deref(),
            Some("2026-10-17T13:19:29Z")
        );
        // `ps` pads a one-digit day (and may pad more).
        assert_eq!(
            start_rfc3339("Fri  Oct  2 03:04:05   2026").as_deref(),
            Some("2026-10-02T03:04:05Z")
        );
        for bad in [
            "",
            "soon",
            "2026-10-17T13:19:29Z",
            "Sat Oct 32 13:19:29 2026",
        ] {
            assert_eq!(start_rfc3339(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn same_start_is_one_instant_to_the_second_in_either_spelling() {
        let rfc = "2026-10-17T13:19:29Z";
        let lstart = "Sat Oct 17 13:19:29 2026";
        assert!(same_start(rfc, lstart));
        assert!(same_start(lstart, rfc));
        assert!(same_start(rfc, rfc));
        assert!(same_start(lstart, "Sat  Oct 17 13:19:29  2026"));
        // Another offset, the same instant; sub-seconds do not count.
        assert!(same_start(rfc, "2026-10-17T15:19:29+02:00"));
        assert!(same_start(rfc, "2026-10-17T13:19:29.900Z"));
        assert!(same_start(&format!(" {rfc} "), rfc));
        // A second apart is another process.
        assert!(!same_start(rfc, "2026-10-17T13:19:30Z"));
        assert!(!same_start(lstart, "Sat Oct 17 13:19:28 2026"));
        // Unreadable on either side fails closed — even the same text twice.
        for bad in ["", "soon", "2026-10-17"] {
            assert!(!same_start(bad, rfc), "{bad:?}");
            assert!(!same_start(rfc, bad), "{bad:?}");
            assert!(!same_start(bad, bad), "{bad:?}");
        }
    }

    #[test]
    fn a_ps_line_is_one_process_read_whole() {
        let p = parse_proc(
            "  412   400   412   412 S+   ttys003  Sat Oct 17 13:19:29 2026     claude --resume x",
        )
        .unwrap();
        assert_eq!(
            p,
            Proc {
                pid: 412,
                ppid: 400,
                pgid: 412,
                tpgid: 412,
                stat: "S+".into(),
                tty: Some("ttys003".into()),
                start: Some("2026-10-17T13:19:29Z".into()),
                command: "claude --resume x".into(),
            }
        );
        assert!(p.in_front());
        // Linux spells it `pts/3`, and no terminal `?`; a one-digit day is padded.
        let q = parse_proc("7 1 7 -1 Ss ? Fri Oct  2 03:04:05 2026 init").unwrap();
        assert_eq!(
            (q.tty, q.start.as_deref()),
            (None, Some("2026-10-02T03:04:05Z"))
        );
        assert!(
            parse_proc("8 1 8 8 S+ pts/3 Fri Oct  2 03:04:05 2026 sh")
                .unwrap()
                .tty
                == Some("pts/3".into())
        );
        // An unreadable start is no start; a short or garbled line is no process.
        assert_eq!(
            parse_proc("9 1 9 9 S+ pts/3 soon soon soon soon soon x")
                .unwrap()
                .start,
            None
        );
        for bad in [
            "",
            "9 1 9 9 S+ pts/3",
            "x 1 9 9 S+ pts/3 Fri Oct 2 03:04:05 2026 sh",
        ] {
            assert_eq!(parse_proc(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn only_a_running_foreground_process_is_in_front() {
        let p = |pgid: i64, tpgid: i64, stat: &str, tty: Option<&str>| Proc {
            pid: 5,
            ppid: 1,
            pgid,
            tpgid,
            stat: stat.into(),
            tty: tty.map(Into::into),
            start: None,
            command: String::new(),
        };
        for stat in ["S+", "R+", "Ss+", "S<+"] {
            assert!(p(5, 5, stat, Some("ttys001")).in_front(), "{stat}");
        }
        // Stopped (Ctrl-Z, in front or not), traced, zombie.
        for stat in ["T", "T+", "t+", "Z+"] {
            assert!(!p(5, 5, stat, Some("ttys001")).in_front(), "{stat}");
        }
        // Another group in front (an editor of its own, a job); no terminal at all.
        assert!(!p(5, 9, "S", Some("ttys001")).in_front());
        assert!(!p(5, 5, "S+", None).in_front());
    }

    #[test]
    fn lsof_names_the_pids_reading_the_terminal() {
        let out = "p10\nn/dev/ttys004\np11\nnpipe\np12\nn/dev/ttys0040\np13\nn/dev/ttys004\n";
        let (listed, hits) = stdin_on(out, "/dev/ttys004");
        assert_eq!((listed, hits), (vec![10, 11, 12, 13], vec![10, 13]));
        assert_eq!(stdin_on("", "/dev/ttys004"), (vec![], vec![]));
    }

    #[test]
    fn a_command_past_its_deadline_is_killed_and_an_error() {
        let t0 = std::time::Instant::now();
        let e = output_within(
            Command::new("sleep").arg("30"),
            std::time::Duration::from_millis(200),
        )
        .unwrap_err();
        assert_eq!(e.kind(), std::io::ErrorKind::TimedOut);
        assert!(t0.elapsed() < std::time::Duration::from_secs(5));
        let out = output_within(
            Command::new("echo").arg("hi"),
            std::time::Duration::from_secs(5),
        )
        .unwrap();
        assert!(out.status.success());
        assert_eq!(out.stdout, b"hi\n");
    }

    // Claude Code 2.1.308 writes `shell` for an idle session with a background shell running.
    #[test]
    fn shell_is_idle_and_nothing_else_is() {
        assert!(is_idle("idle") && is_idle(SHELL));
        for status in ["busy", "waiting", "unknown", "", "Idle", "shell "] {
            assert!(!is_idle(status), "{status:?}");
        }
        let s = Session {
            status: SHELL.into(),
            ..Default::default()
        };
        assert!(s.is_idle() && !s.is_waiting());
        assert!(!Session::default().is_idle());
    }

    // Without a screen to read (iTerm, a pane tmux would not capture), a pending escalated
    // command is taken as the approval prompt it almost always is.
    #[test]
    fn a_codex_session_without_a_readable_screen_waits_on_a_pending_escalation() {
        let busy = |pending| harness::State {
            status: "busy".into(),
            escalation_pending: pending,
            ..Default::default()
        };
        let mut st = busy(true);
        codex_prompt_from_screen(&mut st, None);
        assert_eq!(st.status, "waiting");
        assert_eq!(st.waiting_for.as_deref(), Some("exec approval"));
        let mut st = busy(false);
        codex_prompt_from_screen(&mut st, None);
        assert_eq!(st.status, "busy");
        // A pane that does not exist cannot be read: the hint still decides.
        let mut st = busy(true);
        codex_prompt_from_screen(&mut st, Some("%999999"));
        assert_eq!(st.status, "waiting");
    }

    // `ask_claude` shells out to `claude -p`, and that child registers itself as a
    // live `interactive` session in `$TMPDIR`. Titling the fleet would otherwise
    // discover its own children — and pay for a title for each one.
    #[test]
    fn headless_runs_are_not_sessions() {
        for e in [
            "sdk-cli",
            "sdk-ts",
            "sdk-py",
            "local-agent",
            "github-action",
        ] {
            assert!(is_headless(e), "{e}");
        }
        // A terminal, an editor, a desktop app and anything new: all real fleet
        // members, and an unknown entrypoint is kept rather than dropped.
        for e in [
            "cli",
            "claude-vscode",
            "claude-desktop",
            "remote",
            "whatever",
        ] {
            assert!(!is_headless(e), "{e}");
        }
    }

    #[test]
    fn the_headline_is_the_title_and_falls_back_to_the_label() {
        let mut s = Session {
            pid: 42,
            session_id: Some("aaaaaaaa-1111".into()),
            name: Some("app-9d".into()),
            name_source: Some("derived".into()),
            tab: Some("✳ a tab title".into()),
            ..Default::default()
        };
        // No title yet: the row still needs something to say.
        assert_eq!(s.headline(), "app-9d");
        s.gen_title = Some("statusline-blank".into());
        assert_eq!(s.headline(), "statusline-blank");
        // A blank title is not a title — a row must never draw an empty headline.
        s.gen_title = Some("  ".into());
        assert_eq!(s.headline(), "app-9d");
        // Nothing at all: the short sessionId, then the pid.
        s.gen_title = None;
        s.name = None;
        assert_eq!(s.headline(), "aaaaaaaa");
        // A name somebody chose outranks the generated title: it is the title.
        s.name = Some("fix-login".into());
        s.name_source = Some("user".into());
        s.gen_title = Some("statusline-blank".into());
        assert_eq!(s.headline(), "fix-login");
        s.name = None;
        s.gen_title = None;
        s.session_id = None;
        assert_eq!(s.headline(), "42");
    }

    // The fleet views print titles, so a title has to be usable as a target —
    // "peek the one called statusline-blank" is the whole point of showing it.
    fn titled(pid: i64, name: &str, title: &str) -> Session {
        Session {
            pid,
            name: Some(name.into()),
            name_source: Some("derived".into()),
            gen_title: Some(title.into()),
            ..Default::default()
        }
    }

    #[test]
    fn a_target_resolves_by_title_name_id_or_pid() {
        let row = |pid: i64, name: &str, sid: &str, title: Option<&str>| Session {
            pid,
            name: Some(name.into()),
            session_id: Some(sid.into()),
            gen_title: title.map(str::to_string),
            ..Default::default()
        };
        let rows = vec![
            row(1, "app-9d", "aaaaaaaa-1111", Some("statusline-blank")),
            row(2, "docs-refresh", "bbbbbbbb-2222", None),
        ];
        let one = |target: &str| resolve_in(rows.clone(), target).map(|s| s.pid);
        assert_eq!(one("statusline-blank"), Ok(1));
        assert_eq!(one("app-9d"), Ok(1));
        assert_eq!(one("aaaaaaaa"), Ok(1));
        assert_eq!(one("1"), Ok(1));
        assert_eq!(one("docs-refresh"), Ok(2));
        assert!(one("nothing-like-this").is_err());
    }

    // Typing the whole 30-character title to peek at something is not a workflow.
    #[test]
    fn a_target_can_be_a_fragment_of_the_headline() {
        let rows = vec![titled(1, "app-74", "fleet-llm-title")];
        let one = |q: &str| resolve_in(rows.clone(), q).map(|s| s.pid);
        // Prefix, then substring, then the query's characters in order.
        assert_eq!(one("fleet"), Ok(1));
        assert_eq!(one("llm"), Ok(1));
        assert_eq!(one("fltitle"), Ok(1));
        assert_eq!(one("flt"), Ok(1));
        // Case is not part of anyone's memory of a name.
        assert_eq!(one("FLEET-LLM"), Ok(1));
        // Two characters is not a name — it's a typo, and the looser the rung the
        // more a stray hit costs.
        assert!(one("ft").is_err());
    }

    // The ladder's whole point: a target typed in full is never reinterpreted as a
    // fuzzy match on somebody else. `api` is `api`, even though it is also a
    // subsequence of `a-parallel-index`.
    #[test]
    fn a_tighter_match_wins_outright() {
        let rows = vec![
            titled(1, "api", "api"),
            titled(2, "app-31", "a-parallel-index"),
        ];
        assert_eq!(resolve_in(rows.clone(), "api").map(|s| s.pid), Ok(1));
        // …and the loose rung is still there for a query nothing tighter explains.
        assert_eq!(resolve_in(rows, "aprlx").map(|s| s.pid), Ok(2));
    }

    // Nothing here picks a "best" match: `send` and `rename` type into a live
    // session, so an ambiguous fragment has to come back as a question.
    #[test]
    fn an_ambiguous_fragment_is_an_error_not_a_guess() {
        let rows = vec![
            titled(1, "app-1", "docs-refresh-frontend"),
            titled(2, "app-2", "docs-refresh-api"),
        ];
        let err = resolve_in(rows, "docs-refresh").unwrap_err();
        assert!(err.contains("matches 2 sessions"), "{err}");
        assert!(err.contains("docs-refresh-frontend"), "{err}");
        assert!(err.contains("docs-refresh-api"), "{err}");
    }

    // The bug the exact forms exist for: the pid of a session that has just exited is
    // no longer anyone's pid, so a bare pid walks on down the ladder and lands on a
    // title that happens to carry those digits — and `send` types into it.
    #[test]
    fn an_exact_pid_never_falls_through_to_a_title() {
        let rows = vec![titled(7, "app-7", "retry-4242-uploads")];
        // Bare: still the ladder, for compatibility (substring of the title).
        assert_eq!(resolve_in(rows.clone(), "4242").map(|s| s.pid), Ok(7));
        // Exact: pid 4242 is gone, so nothing — never the title.
        let err = resolve_target_in(rows.clone(), "pid:4242").unwrap_err();
        assert_eq!(err.kind(), "target_not_found");
        assert!(
            err.to_string().contains("no live session has pid 4242"),
            "{err}"
        );
        // …and the live pid resolves, whatever the title says.
        assert_eq!(resolve_in(rows.clone(), "pid:7").map(|s| s.pid), Ok(7));
        assert_eq!(resolve_in(rows, " PID: 7 ").map(|s| s.pid), Ok(7));
    }

    #[test]
    fn an_exact_id_is_the_whole_session_id() {
        let row = |pid: i64, sid: &str, title: &str| Session {
            pid,
            session_id: Some(sid.into()),
            gen_title: Some(title.into()),
            ..Default::default()
        };
        let sid = "aaaaaaaa-1111-2222-3333-444444444444";
        let rows = vec![
            row(1, sid, "statusline-blank"),
            row(2, "bbbbbbbb-1111-2222-3333-444444444444", "aaaaaaaa-notes"),
        ];
        let one = |q: &str| resolve_target_in(rows.clone(), q).map(|s| s.pid);
        assert_eq!(one(&format!("id:{sid}")), Ok(1));
        assert_eq!(one(&format!("ID:{}", sid.to_uppercase())), Ok(1));
        // A prefix is enough for a bare target, never for `id:`.
        assert_eq!(one("aaaaaaaa-1111").map(|p| p == 1), Ok(true));
        assert_eq!(one("id:aaaaaaaa").unwrap_err().kind(), "target_not_found");
        // A title is not an id, even one that looks like one.
        assert_eq!(
            one("id:aaaaaaaa-notes").unwrap_err().kind(),
            "target_not_found"
        );
        // The same id twice (a resumed session in two terminals) is a question, not a pick.
        let twice = vec![row(1, sid, "a"), row(2, sid, "b")];
        let err = resolve_target_in(twice, &format!("id:{sid}")).unwrap_err();
        assert_eq!(err.kind(), "target_ambiguous");
        assert!(err.to_string().contains("matches 2 sessions"), "{err}");
    }

    // One `ps eww -o pid=,tty=,lstart=,command=` line per live pid: what the exact lookup
    // knows of a process, and how it tells a reused pid from the registered one.
    #[test]
    fn one_ps_line_carries_tty_start_and_environment() {
        let line = "  4242 ttys012  Fri Oct 16 13:44:45 2026     claude --resume x TMUX=/tmp/t,1,0 TERM=xterm";
        let (pid, p) = parse_inspected(line).unwrap();
        assert_eq!(pid, 4242);
        assert_eq!(p.tty, "ttys012");
        assert!(p.env.contains(" TMUX=/tmp/t,1,0"), "{}", p.env);
        // Claude's `procStart` is the same lstart in UTC; padding differs, the time doesn't.
        assert!(p.started_as("Fri Oct 16 13:44:45 2026"));
        assert!(p.started_as("Fri Oct 16  13:44:45  2026"));
        assert!(!p.started_as("Fri Oct 16 13:44:46 2026"), "a newer process");
        assert!(!p.started_as("Mon Jan  1 00:00:00 2001"));
        // UTC only: the same wall time read as some other zone's is a different instant.
        assert!(!p.started_as("Fri Oct 16 15:44:45 2026"));
        // Not a `ps` time: it must then be the same string, or the check fails closed.
        assert!(!p.started_as("2026-10-16T13:44:45Z"));
        assert!(!p.started_as("yesterday"));
        let (_, odd) = parse_inspected("  9 ttys001 Fri Oct 16 13:44:45 2026x sh").unwrap();
        assert!(odd.started_as("Fri  Oct 16 13:44:45 2026x"));
        assert!(!odd.started_as("Fri Oct 16 13:44:45 2026"));
        // No terminal, or not a process line: absent.
        assert!(parse_inspected("  77 ??  Fri Oct 16 13:44:45 2026 launchd").is_none());
        assert!(parse_inspected("garbage").is_none());
    }

    #[test]
    fn malformed_exact_forms_are_errors_and_other_colons_are_bare() {
        let rows = vec![titled(7, "app:7", "fix-login")];
        for bad in ["pid:", "pid:abc", "pid:-3", "pid:0", "id:", "id:  "] {
            let err = resolve_target_in(rows.clone(), bad).unwrap_err();
            assert_eq!(err.kind(), "target_invalid", "{bad}: {err}");
        }
        assert_eq!(exact_target("pid:12"), Some(Ok(Exact::Pid(12))));
        assert_eq!(exact_target("id:abc"), Some(Ok(Exact::Id("abc".into()))));
        // Any other prefix is a bare target and walks the ladder as before.
        assert_eq!(exact_target("app:7"), None);
        assert_eq!(exact_target("fix-login"), None);
        assert_eq!(resolve_in(rows.clone(), "app:7").map(|s| s.pid), Ok(7));
        assert_eq!(
            resolve_target_in(rows, "").unwrap_err().kind(),
            "target_invalid"
        );
    }

    #[test]
    fn a_subsequence_needs_its_characters_in_order() {
        assert!(is_subsequence("flt", "fleet-llm-title"));
        assert!(is_subsequence("fleetllmtitle", "fleet-llm-title"));
        // Right characters, wrong order.
        assert!(!is_subsequence("tlf", "fleet-llm-title"));
        // A character the haystack simply doesn't have.
        assert!(!is_subsequence("fltz", "fleet-llm-title"));
        // Repeated characters need that many occurrences left to consume.
        assert!(!is_subsequence("fff", "fleet-llm-title"));
    }

    // Two sessions can land on the same title. The error has to name them by what
    // the user actually saw, which is the title.
    #[test]
    fn an_ambiguous_target_lists_the_headlines() {
        let dup = |pid: i64| Session {
            pid,
            name: Some(format!("work-{pid}")),
            name_source: Some("derived".into()),
            gen_title: Some("docs-refresh".into()),
            ..Default::default()
        };
        let err = resolve_in(vec![dup(1), dup(2)], "docs-refresh").unwrap_err();
        assert!(err.contains("matches 2 sessions"), "{err}");
        assert!(err.contains("docs-refresh, docs-refresh"), "{err}");
    }

    #[test]
    fn cwd_encoding() {
        assert_eq!(encode_cwd("/home/user/Code/app"), "-home-user-Code-app");
        assert_eq!(encode_cwd("/a/b.c"), "-a-b-c");
    }

    #[test]
    fn panes_carry_their_tmux_session() {
        let (tty, p) = parse_pane("/dev/ttys004\tapi-server:2.0\t%17\tzsh\tapi-server").unwrap();
        assert_eq!(tty, "/dev/ttys004");
        assert_eq!(p.session_name, "api-server");
        assert_eq!(p.window_name, "zsh");
        assert_eq!(p.pane_id, "%17");
        // A session name containing a colon still resolves, because we don't
        // reconstruct it from `target`.
        let (_, p) = parse_pane("/dev/ttys005\ta:b:1.0\t%1\tvim\ta:b").unwrap();
        assert_eq!(p.session_name, "a:b");
        assert!(parse_pane("garbage").is_none());
    }

    #[test]
    fn backend_falls_back_to_the_tmux_pane_owning_the_tty() {
        let panes: HashMap<String, Pane> = parse_pane("/dev/ttys020\tjob:1.0\t%7\tzsh\tjob")
            .into_iter()
            .collect();
        // Env unreadable, tty is a pane's: tmux.
        assert_eq!(
            backend_from_env("", Some("ttys020"), &panes),
            (Backend::Tmux, Some("%7".into()))
        );
        // Env unreadable, tty unknown: nothing to go on.
        assert_eq!(
            backend_from_env("", Some("ttys099"), &panes),
            (Backend::Unknown, None)
        );
        // iTerm env wins over a stray tty match only when TMUX is absent.
        assert_eq!(
            backend_from_env("x ITERM_SESSION_ID=w0t0p0:ABC", Some("ttys099"), &panes),
            (Backend::Iterm, Some("ABC".into()))
        );
        assert_eq!(
            backend_from_env("x TMUX=/tmp/s,1,0", Some("ttys020"), &panes).0,
            Backend::Tmux
        );
    }

    #[test]
    fn derived_names_are_flagged() {
        let mut s = Session::default();
        assert!(!s.is_derived_name());
        s.name_source = Some("derived".into());
        assert!(s.is_derived_name());
        s.name_source = Some("user".into());
        assert!(!s.is_derived_name());
    }
}
