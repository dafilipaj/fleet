//! Session discovery. The authoritative source is Claude's own live registry at
//! `~/.claude/sessions/<pid>.json` (precise pid -> sessionId -> cwd -> status).
//! Each entry is enriched with the process tty and the backend it runs in.

use std::collections::HashMap;
use std::path::PathBuf;
use std::process::Command;

use serde::{Deserialize, Serialize};

use crate::core::harness::{self, Harness};

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
    pub status: String,
    /// Milliseconds since the epoch of the last status update.
    pub updated_at: Option<i64>,
    /// Milliseconds since the epoch of the last human prompt ([`crate::core::prompted`]);
    /// `null` when unknown (no prompt yet, or a Codex / pi session).
    pub prompted_at: Option<i64>,
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
    /// Subagents and background shells it runs ([`crate::core::background`]); `null` when
    /// none (or unknown). With them, a session whose main turn has ended is `idle`.
    pub background: Option<crate::core::background::Background>,
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
    /// Absolute path of the session's transcript, when known (Codex / pi only — a
    /// Claude transcript is found from cwd + session id).
    pub transcript: Option<String>,
    /// The [job](crate::core::jobs) run that started it (`{ job, run }`); `null` when none.
    pub origin: Option<crate::core::jobs::Origin>,
    /// The GitHub identity it acts as ([`crate::core::gh_identity`]); `null` = the user's own.
    pub gh_identity: Option<String>,
    /// The app whose embedded Fleet panel started it (the web embed view's `app`, the pane's
    /// `@fleet_embed` option); `null` when none.
    pub embed: Option<String>,
    /// The git checkout its cwd is in (`{ branch, root, linked, repo }`, see
    /// [`crate::core::git`]); `null` outside one. Stamped by `list`; `None` inside discovery.
    pub git: Option<crate::core::git::GitHead>,
    /// It reported its work finished (`fleet done`: `{ at, note }`) and was not prompted since;
    /// `null` otherwise. Stamped by `list`; `None` inside discovery.
    pub done: Option<crate::core::done::DoneRef>,
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
            prompted_at: None,
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
            background: None,
            context: None,
            stack: None,
            harness: Harness::Claude,
            model: None,
            transcript: None,
            origin: None,
            gh_identity: None,
            embed: None,
            git: None,
            done: None,
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
    /// When this process started (ms): what it launched before is not running.
    #[serde(rename = "startedAt")]
    started_at: Option<i64>,
    #[serde(rename = "nameSource")]
    name_source: Option<String>,
    #[serde(rename = "waitingFor")]
    waiting_for: Option<String>,
    /// How this session was started — `"cli"` for a terminal, `"sdk-cli"` for a
    /// `claude -p` child. See [`is_headless`].
    entrypoint: Option<String>,
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
    /// The pane's `@fleet_gh_identity` option: the GitHub identity it acts as (empty when none).
    pub gh_identity: String,
    /// The pane's `@fleet_embed` option: the app whose embedded panel started it (empty when none).
    pub embed: String,
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
        let path = entry.path();
        if path.extension().and_then(|s| s.to_str()) != Some("json") {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        let Ok(reg) = serde_json::from_str::<Reg>(&text) else {
            continue;
        };
        let Some(pid) = reg.pid else { continue };
        // Interactive sessions only (skip print/headless runs). `kind` alone does
        // not say it: a `claude -p` child registers as `interactive` too, and its
        // entrypoint is the only thing that gives it away.
        if reg.kind.as_deref().is_some_and(|k| k != "interactive") {
            continue;
        }
        if reg.entrypoint.as_deref().is_some_and(is_headless) {
            continue;
        }
        // ps returns non-zero for a dead pid, so this doubles as a liveness check.
        let Some(tty) = tty_of(pid) else { continue };
        let (backend, handle) = backend_of(pid, Some(tty.as_str()), &panes);
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
        let title = match (reg.session_id.as_deref(), reg.cwd.as_deref()) {
            (Some(sid), Some(cwd)) => title_for(cwd, sid),
            _ => None,
        };
        let context = reg.session_id.as_deref().and_then(|sid| {
            crate::core::context::for_session(&claude_home(), reg.cwd.as_deref(), sid)
        });
        let prompted_at = reg.session_id.as_deref().and_then(|sid| {
            crate::core::prompted::for_session(&claude_home(), reg.cwd.as_deref(), sid)
        });
        let mut status = reg.status.unwrap_or_else(|| "unknown".into());
        let mut updated_at = reg.status_updated_at.or(reg.updated_at);
        // `busy` / `shell` also while only subagents or background shells run: the main
        // thread may be free (see core::background).
        let mut background = None;
        if status == "busy" || status == "shell" {
            if let Some(m) = reg.session_id.as_deref().and_then(|sid| {
                crate::core::background::for_session(
                    &claude_home(),
                    reg.cwd.as_deref(),
                    sid,
                    reg.started_at,
                )
            }) {
                if let Some(ended) = m.ended_at
                    && (!m.background.is_empty() || status == "shell")
                {
                    status = "idle".into();
                    updated_at = Some(ended);
                }
                background = (!m.background.is_empty()).then_some(m.background);
            }
            // `shell`: only background shells run — the main thread is free.
            if status == "shell" {
                status = "idle".into();
            }
        }
        out.push(Session {
            pid,
            status,
            updated_at,
            background,
            prompted_at,
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
            model: context.as_ref().and_then(|c| c.model.clone()),
            context,
            stack: None,
            harness: Harness::Claude,
            transcript: None,
            origin,
            gh_identity: pane
                .map(|p| p.gh_identity.clone())
                .filter(|g| !g.is_empty()),
            embed: pane.map(|p| p.embed.clone()).filter(|e| !e.is_empty()),
            git: None,
            done: None,
        });
    }
    crate::core::context::apply_native_1m(out.iter_mut().filter_map(|s| s.context.as_mut()));
    out.extend(discover_harnesses(&panes));
    out.sort_by_key(|s| std::cmp::Reverse(s.updated_at.unwrap_or(0)));
    Ok(out)
}

/// Live Codex and pi sessions (see [`crate::core::harness`]): found with `ps`, matched
/// to their transcripts, placed in their terminal like Claude's.
fn discover_harnesses(panes: &HashMap<String, Pane>) -> Vec<Session> {
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
            let mut s = harness_session(p, assigned.get(&p.pid), panes);
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
    let state = t
        .map(|t| harness::state(p.harness, &t.path))
        .unwrap_or_else(|| harness::State {
            status: "idle".into(),
            ..Default::default()
        });
    Session {
        pid: p.pid,
        name_source: state.name.is_some().then(|| "user".to_string()),
        name: state.name,
        session_id: t.map(|t| t.session_id.clone()),
        cwd: t.map(|t| t.cwd.clone()).or_else(|| p.cwd.clone()),
        status: state.status,
        waiting_for: state.waiting_for,
        updated_at: Some(t.map(|t| t.mtime_ms).unwrap_or(p.start_ms)),
        prompted_at: None,
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
        gh_identity: pane
            .map(|p| p.gh_identity.clone())
            .filter(|g| !g.is_empty()),
        embed: pane.map(|p| p.embed.clone()).filter(|e| !e.is_empty()),
        title: t.and_then(|t| harness::first_prompt(p.harness, &t.path)),
        model: state.model.or_else(|| p.model_arg()),
        context: state.context,
        harness: p.harness,
        transcript: t.map(|t| t.path.display().to_string()),
        ..Default::default()
    }
}

/// Resolve a target to exactly one session.
///
/// A target is a pid, a sessionId prefix, a session name, a generated title — or
/// any unambiguous fragment of the last two, down to a fuzzy `flt` for
/// `fleet-llm-title`. See [`resolve_in`] for how loose is too loose.
pub fn resolve(target: &str) -> Result<Session, String> {
    let mut rows = discover();
    // Every fleet view prints the *title* as a session's identity, so a title has
    // to resolve: otherwise `peek <the thing list just showed me>` fails for every
    // titled session, which is all of them.
    crate::core::naming::stamp_titles(&mut rows);
    resolve_in(rows, target)
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
    let q = target.trim().to_lowercase();
    if q.is_empty() {
        return Err("no target given".into());
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
        return Err(format!(
            "\"{target}\" matches {} sessions: {} — be more specific",
            all.len(),
            all.join(", ")
        ));
    }
    Err(format!("no live session matches \"{target}\""))
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
            "#{pane_tty}\t#{session_name}:#{window_index}.#{pane_index}\t#{pane_id}\t#{window_name}\t#{session_name}\t#{@fleet_job_run}\t#{@fleet_gh_identity}\t#{@fleet_embed}",
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

/// `pane_tty \t target \t pane_id \t window_name \t session_name \t job_run \t gh_identity \t embed` -> (tty, pane).
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
    let gh_identity = parts.next().unwrap_or("").to_string();
    let embed = parts.next().unwrap_or("").to_string();
    Some((
        ptty.to_string(),
        Pane {
            target: target.to_string(),
            pane_id: pane_id.to_string(),
            window_name,
            session_name,
            job_run,
            gh_identity,
            embed,
        },
    ))
}

/// Fill iTerm sessions' `tab` with their tab title via a single AppleScript call.
/// Kept out of `discover()` so one-shot commands (peek/send) don't pay for it.
pub fn enrich_iterm_tabs(rows: &mut [Session]) {
    if is_fixture() {
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
tell application id "com.googlecode.iterm2"
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

/// Title = the session's first genuine user prompt from its transcript.
fn title_for(cwd: &str, session_id: &str) -> Option<String> {
    claude_prompts(cwd, session_id, 1).into_iter().next()
}

/// The first `n` genuine user prompts of a session, in order: what naming reads past the title.
pub fn opening_prompts(s: &Session, n: usize) -> Vec<String> {
    match (s.harness, s.session_id.as_deref(), s.cwd.as_deref()) {
        (Harness::Claude, Some(sid), Some(cwd)) => claude_prompts(cwd, sid, n),
        (Harness::Claude, ..) => Vec::new(),
        (h, ..) => s
            .transcript
            .as_deref()
            .map(|p| harness::prompts(h, std::path::Path::new(p), n))
            .unwrap_or_default(),
    }
}

/// The first `n` genuine user prompts of a Claude transcript: no slash commands, no
/// `<…>`-wrapped injections, no interrupt markers.
fn claude_prompts(cwd: &str, session_id: &str, n: usize) -> Vec<String> {
    let mut out = Vec::new();
    let file = claude_home()
        .join("projects")
        .join(encode_cwd(cwd))
        .join(format!("{session_id}.jsonl"));
    let Ok(text) = std::fs::read_to_string(&file) else {
        return out;
    };
    for line in text.lines().take(2000) {
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
        if !t.is_empty()
            && !t.starts_with('<')
            && !t.starts_with('/')
            && !t.starts_with("[Request interrupted")
        {
            out.push(t.to_string());
            if out.len() >= n {
                break;
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

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
    fn panes_carry_their_fleet_options() {
        let (_, p) = parse_pane("/dev/ttys006	fw-1:0.0	%2	zsh	fw-1	nightly/r1	bot	fin").unwrap();
        assert_eq!(p.job_run, "nightly/r1");
        assert_eq!(p.gh_identity, "bot");
        assert_eq!(p.embed, "fin");
        let (_, p) = parse_pane("/dev/ttys006	fw-1:0.0	%2	zsh	fw-1		").unwrap();
        assert_eq!(p.embed, "");
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
