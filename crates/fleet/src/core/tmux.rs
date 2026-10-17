//! tmux *sessions* (not Claude sessions): list, match, create, attach, kill,
//! rename, and the stale-session classifier.
//!
//! Everything here runs against the local tmux server. Remote hosts are reached
//! one level up, by re-running `fleet` there (see [`crate::core::hosts`]).

use std::collections::{HashMap, HashSet};
use std::process::Command;

use serde::{Deserialize, Serialize};

use crate::core::tools;
use crate::error::{Error, Result};

fn tmux() -> Command {
    tools::tmux_cmd()
}

/// Is a tmux binary available at all?
pub fn available() -> bool {
    tmux()
        .arg("-V")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

pub fn version() -> Option<String> {
    let o = tmux().arg("-V").output().ok()?;
    o.status
        .success()
        .then(|| String::from_utf8_lossy(&o.stdout).trim().to_string())
}

fn require() -> Result<()> {
    if available() {
        Ok(())
    } else {
        Err(Error::Other(format!(
            "tmux not found (looked for {}; set `tmux` in the config or FLEET_TMUX)",
            tools::tmux()
        )))
    }
}

/// One tmux session as `list-sessions` reports it.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct TmuxSession {
    pub name: String,
    /// Clients attached right now.
    pub attached: i64,
    pub windows: i64,
    /// Epoch seconds; 0 = never attached.
    pub last_attached: i64,
    pub activity: i64,
    pub created: i64,
}

const LIST_FMT: &str = "#{session_last_attached}|#{session_attached}|#{session_windows}|#{session_activity}|#{session_created}|#{session_name}";

/// Parse one `LIST_FMT` line. The name comes last and keeps any `|` it contains.
fn parse_session(line: &str) -> Option<TmuxSession> {
    let mut parts = line.splitn(6, '|');
    let num = |s: Option<&str>| s.and_then(|v| v.trim().parse::<i64>().ok()).unwrap_or(0);
    let last_attached = num(parts.next());
    let attached = num(parts.next());
    let windows = num(parts.next());
    let activity = num(parts.next());
    let created = num(parts.next());
    let name = parts.next()?.to_string();
    if name.is_empty() {
        return None;
    }
    Some(TmuxSession {
        name,
        attached,
        windows,
        last_attached,
        activity,
        created,
    })
}

/// Most recently attached first, then most recently active.
pub fn sort_sessions(v: &mut [TmuxSession]) {
    v.sort_by(|a, b| {
        b.last_attached
            .cmp(&a.last_attached)
            .then(b.activity.cmp(&a.activity))
    });
}

/// Every session on the local server, sorted. Empty when no server is running.
pub fn list_sessions() -> Result<Vec<TmuxSession>> {
    require()?;
    let out = tmux().args(["list-sessions", "-F", LIST_FMT]).output()?;
    // "no server running" / "error connecting" is an empty server, not a failure.
    let mut v: Vec<TmuxSession> = String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(parse_session)
        .collect();
    sort_sessions(&mut v);
    Ok(v)
}

/// The tmux session this process runs inside, if any.
pub fn current_session() -> Option<String> {
    std::env::var_os("TMUX")?;
    let o = tmux().args(["display-message", "-p", "#S"]).output().ok()?;
    let s = String::from_utf8_lossy(&o.stdout).trim().to_string();
    (!s.is_empty()).then_some(s)
}

// --- matching ----------------------------------------------------------------

/// Names matching `q`: exact > exact (case-folded) > prefix > substring. The
/// first tier with any hit decides.
pub fn matches(sessions: &[TmuxSession], q: &str) -> Vec<String> {
    match_tier(sessions, q).map(|(_, v)| v).unwrap_or_default()
}

/// [`matches`] with the tier that decided (0 = exact … 3 = substring), so hits
/// from several hosts can be ranked against each other.
pub fn match_tier(sessions: &[TmuxSession], q: &str) -> Option<(usize, Vec<String>)> {
    let ql = q.to_lowercase();
    let tiers: [&dyn Fn(&str) -> bool; 4] = [
        &|n: &str| n == q,
        &|n: &str| n.to_lowercase() == ql,
        &|n: &str| n.to_lowercase().starts_with(&ql),
        &|n: &str| n.to_lowercase().contains(&ql),
    ];
    for (i, tier) in tiers.iter().enumerate() {
        let hits: Vec<String> = sessions
            .iter()
            .filter(|s| tier(&s.name))
            .map(|s| s.name.clone())
            .collect();
        if !hits.is_empty() {
            return Some((i, hits));
        }
    }
    None
}

#[derive(Debug, PartialEq, Eq)]
pub enum Resolve {
    One(String),
    None,
    Many(Vec<String>),
}

pub fn resolve_in(sessions: &[TmuxSession], q: &str) -> Resolve {
    // An empty query would match everything through the substring tier.
    if q.is_empty() {
        return Resolve::None;
    }
    let mut hits = matches(sessions, q);
    match hits.len() {
        0 => Resolve::None,
        1 => Resolve::One(hits.remove(0)),
        _ => Resolve::Many(hits),
    }
}

/// The session to jump back to: the most recently attached one that isn't `cur`.
pub fn last_target(sessions: &[TmuxSession], cur: Option<&str>) -> Option<String> {
    sessions
        .iter()
        .find(|s| Some(s.name.as_str()) != cur)
        .map(|s| s.name.clone())
}

/// Make a user-typed name safe as a tmux session name: anything outside
/// `[A-Za-z0-9_-]` becomes `-`, runs collapse, and edges are trimmed.
pub fn sanitize_name(s: &str) -> String {
    let mut out = String::new();
    for c in s.chars() {
        let c = if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
            c
        } else {
            '-'
        };
        if c == '-' && out.ends_with('-') {
            continue;
        }
        out.push(c);
    }
    out.trim_matches('-').to_string()
}

// --- operations --------------------------------------------------------------

fn exact(name: &str) -> String {
    format!("={name}")
}

pub fn has_session(name: &str) -> bool {
    tmux()
        .args(["has-session", "-t", &exact(name)])
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

fn run(args: &[&str]) -> Result<()> {
    if crate::core::hosts::dry_run() {
        let mut shown = tools::shq_min(tools::tmux());
        for a in args {
            shown.push(' ');
            shown.push_str(&tools::shq_min(a));
        }
        println!("{shown}");
        return Ok(());
    }
    let out = tmux().args(args).output()?;
    if !out.status.success() {
        let why = String::from_utf8_lossy(&out.stderr).trim().to_string();
        return Err(Error::Other(if why.is_empty() {
            format!("tmux {} exited {}", args.first().unwrap_or(&""), out.status)
        } else {
            format!("tmux: {why}")
        }));
    }
    Ok(())
}

/// Create a detached session, optionally in `dir` and running `cmd`. Under `FLEET_PATH_ONLY`
/// its panes start with fleet's `PATH` and the switch ([`tools::pane_env`]), not the server's.
pub fn new_session(name: &str, dir: Option<&str>, cmd: &[String]) -> Result<()> {
    require()?;
    let dir = dir.map(tools::expand_tilde);
    let args = new_session_args(name, dir.as_deref(), &tools::pane_env(), cmd);
    run(&args.iter().map(String::as_str).collect::<Vec<_>>())
}

fn new_session_args(name: &str, dir: Option<&str>, env: &[String], cmd: &[String]) -> Vec<String> {
    let mut args: Vec<String> = ["new-session", "-d", "-s", name].map(String::from).into();
    if let Some(d) = dir {
        args.extend(["-c".into(), d.into()]);
    }
    for kv in env {
        args.extend(["-e".into(), kv.clone()]);
    }
    args.extend(cmd.iter().cloned());
    args
}

pub fn kill_session(name: &str) -> Result<()> {
    run(&["kill-session", "-t", &exact(name)])
}

pub fn rename_session(old: &str, new: &str) -> Result<()> {
    run(&["rename-session", "-t", &exact(old), new])
}

/// Attach to (or, inside tmux, switch to) `name`. Replaces this process when
/// attaching, so the terminal belongs to tmux from here on.
pub fn attach(name: &str) -> Result<()> {
    require()?;
    let target = exact(name);
    let inside = std::env::var_os("TMUX").is_some();
    let args: Vec<&str> = if inside {
        vec!["switch-client", "-t", &target]
    } else {
        vec!["attach", "-t", &target]
    };
    if crate::core::hosts::dry_run() {
        return run(&args);
    }
    if inside {
        return run(&args);
    }
    use std::os::unix::process::CommandExt;
    let err = tmux().args(&args).exec();
    Err(Error::Other(format!("cannot exec tmux: {err}")))
}

// --- stale sessions ----------------------------------------------------------

/// One tmux pane, as the stale check sees it.
#[derive(Debug, Clone, Default)]
pub struct PaneInfo {
    pub pane_id: String,
    pub command: String,
    pub pid: i64,
    pub in_mode: bool,
    /// Without `/dev/`.
    pub tty: String,
    pub session: String,
}

/// One live Claude session, reduced to what places it in tmux.
#[derive(Debug, Clone, Default)]
pub struct ClaudeRef {
    pub tmux_backed: bool,
    pub handle: Option<String>,
    /// Without `/dev/`.
    pub tty: Option<String>,
    pub name: String,
    pub status: String,
}

/// Everything the classifier needs, gathered in one pass.
#[derive(Debug, Clone, Default)]
pub struct StaleInput {
    pub now: i64,
    pub current: Option<String>,
    pub sessions: Vec<TmuxSession>,
    pub panes: Vec<PaneInfo>,
    /// `(pid, ppid, stat)` for every process.
    pub procs: Vec<(i64, i64, String)>,
    /// `None` = the live-Claude cross-check was skipped or failed to run.
    pub claude: Option<std::result::Result<Vec<ClaudeRef>, String>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum KeptClass {
    Here,
    Attached,
    Infra,
    Claude,
    Unknown,
    Running,
    Recent,
}

#[derive(Debug, Clone, Serialize)]
pub struct Kept {
    pub name: String,
    pub reason: String,
    pub class: KeptClass,
}

#[derive(Debug, Clone, Serialize)]
pub struct Candidate {
    pub name: String,
    pub idle_secs: i64,
    pub windows: i64,
    pub command: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct StaleReport {
    /// Idle-longest first.
    pub candidates: Vec<Candidate>,
    pub kept: Vec<Kept>,
    /// `true` when the live-Claude cross-check ran and is trustworthy.
    pub claude_checked: bool,
    /// Why the cross-check is not trustworthy (when it isn't).
    pub claude_problem: Option<String>,
}

const IDLE_SHELLS: [&str; 8] = [
    "zsh", "-zsh", "bash", "-bash", "sh", "login", "fish", "-fish",
];

/// Compact duration: 45s / 12m / 3h / 2d.
pub fn dur(s: i64) -> String {
    let s = s.max(0);
    if s < 60 {
        format!("{s}s")
    } else if s < 3600 {
        format!("{}m", s / 60)
    } else if s < 86400 {
        format!("{}h", s / 3600)
    } else {
        format!("{}d", s / 86400)
    }
}

/// The longest threshold accepted (3650d) — beyond it the number is meaningless.
pub const SPAN_MAX: i64 = 315_360_000;

/// `30m` / `12h` / `7d`, or a bare number of hours → seconds.
pub fn parse_span(v: &str) -> std::result::Result<i64, String> {
    let v = v.trim();
    let (num, unit) = match v.chars().last() {
        Some('m') => (&v[..v.len() - 1], 60),
        Some('h') => (&v[..v.len() - 1], 3600),
        Some('d') => (&v[..v.len() - 1], 86400),
        _ => (v, 3600),
    };
    if num.is_empty() || !num.chars().all(|c| c.is_ascii_digit()) {
        return Err(format!(
            "bad duration '{v}' (use 30m, 12h, 7d, or a number of hours)"
        ));
    }
    let n: i64 = num
        .trim_start_matches('0')
        .parse()
        .or_else(|_| {
            if num.chars().all(|c| c == '0') {
                Ok(0)
            } else {
                Err(())
            }
        })
        .map_err(|_| format!("duration '{v}' is out of range (max 3650d)"))?;
    let secs = n
        .checked_mul(unit)
        .filter(|s| *s <= SPAN_MAX)
        .ok_or_else(|| format!("duration '{v}' is out of range (max 3650d)"))?;
    Ok(secs)
}

/// Classify every session: kept (with the first reason that applies) or a
/// stale candidate.
///
/// The live-Claude cross-check is what makes this safe to act on: a Claude
/// session sitting at its prompt looks exactly like an idle shell from tmux's
/// side. Pane ids get recycled, ttys do not — so a handle/tty disagreement, or a
/// tmux-backed Claude session that maps to no live pane, means the map is stale
/// and the check as a whole is reported untrustworthy rather than guessed at.
pub fn classify(input: &StaleInput, threshold: i64) -> StaleReport {
    let pane_session: HashMap<&str, &str> = input
        .panes
        .iter()
        .map(|p| (p.pane_id.as_str(), p.session.as_str()))
        .collect();
    let tty_session: HashMap<&str, &str> = input
        .panes
        .iter()
        .filter(|p| !p.tty.is_empty())
        .map(|p| (p.tty.as_str(), p.session.as_str()))
        .collect();

    // Which tmux sessions hold a live Claude, and whether the map is sound.
    let mut claude_in: HashMap<String, String> = HashMap::new();
    let (mut checked, mut problem) = (false, None);
    match &input.claude {
        None => problem = Some("skipped".to_string()),
        Some(Err(e)) => problem = Some(e.clone()),
        Some(Ok(refs)) => {
            let (mut mismatch, mut unmapped) = (0, 0);
            for c in refs {
                let desc = format!("{} {}", c.name, c.status);
                let th = c
                    .handle
                    .as_deref()
                    .and_then(|h| pane_session.get(h).copied());
                let tk = c.tty.as_deref().and_then(|t| tty_session.get(t).copied());
                if !c.tmux_backed {
                    // Not ours to verify, but if its tty sits in a pane, protect it.
                    if let Some(s) = tk.or(th) {
                        claude_in.insert(s.to_string(), desc);
                    }
                    continue;
                }
                match (th, tk) {
                    (Some(a), Some(b)) if a != b => {
                        mismatch += 1;
                        claude_in.insert(a.to_string(), desc.clone());
                        claude_in.insert(b.to_string(), desc);
                    }
                    (None, None) => unmapped += 1,
                    (a, b) => {
                        if c.handle.is_some() && c.tty.is_some() && (a.is_none() || b.is_none()) {
                            unmapped += 1;
                        }
                        claude_in.insert(a.or(b).unwrap_or_default().to_string(), desc);
                    }
                }
            }
            if mismatch > 0 {
                problem = Some(format!(
                    "{mismatch} Claude handle/tty disagreement(s) — stale fleet map"
                ));
            } else if unmapped > 0 {
                problem = Some(format!(
                    "{unmapped} Claude session(s) match no live pane — stale fleet map"
                ));
            } else {
                checked = true;
            }
        }
    }

    // Process tree: a pane whose shell has a live child is doing work.
    let mut kids: HashSet<i64> = HashSet::new();
    let mut suspended: HashSet<i64> = HashSet::new();
    for (_, ppid, stat) in &input.procs {
        kids.insert(*ppid);
        if stat.starts_with('T') {
            suspended.insert(*ppid);
        }
    }

    #[derive(Default)]
    struct Agg {
        panes: usize,
        first_cmd: String,
        bad_cmd: Option<String>,
        busy: bool,
        susp: bool,
        mode: bool,
    }
    let mut agg: HashMap<&str, Agg> = HashMap::new();
    for p in &input.panes {
        let a = agg.entry(p.session.as_str()).or_default();
        a.panes += 1;
        if a.first_cmd.is_empty() {
            a.first_cmd = p.command.clone();
        }
        if !IDLE_SHELLS.contains(&p.command.as_str()) && a.bad_cmd.is_none() {
            a.bad_cmd = Some(p.command.clone());
        }
        if p.in_mode {
            a.mode = true;
        }
        if kids.contains(&p.pid) {
            a.busy = true;
            if suspended.contains(&p.pid) {
                a.susp = true;
            }
        }
    }

    let mut kept = Vec::new();
    let mut candidates = Vec::new();
    let empty = Agg::default();
    for s in &input.sessions {
        let la = if s.last_attached > 0 {
            s.last_attached
        } else {
            s.created
        };
        let ac = if s.activity > 0 { s.activity } else { la };
        let age = (input.now - la).max(0).min((input.now - ac).max(0));
        let a = agg.get(s.name.as_str()).unwrap_or(&empty);
        let why: Option<(String, KeptClass)> = if input.current.as_deref() == Some(&s.name) {
            Some(("you are here".into(), KeptClass::Here))
        } else if s.attached > 0 {
            Some(("attached".into(), KeptClass::Attached))
        } else if s.name.eq_ignore_ascii_case("fleet") {
            Some(("infrastructure".into(), KeptClass::Infra))
        } else if let Some(c) = claude_in.get(&s.name) {
            Some((format!("claude: {c}"), KeptClass::Claude))
        } else if a.panes == 0 {
            Some(("no pane info".into(), KeptClass::Unknown))
        } else if let Some(cmd) = &a.bad_cmd {
            Some((format!("running: {cmd}"), KeptClass::Running))
        } else if a.susp {
            Some(("suspended job".into(), KeptClass::Running))
        } else if a.busy {
            Some(("background job".into(), KeptClass::Running))
        } else if a.mode {
            Some(("copy-mode".into(), KeptClass::Running))
        } else if age < threshold {
            Some((format!("idle {}", dur(age)), KeptClass::Recent))
        } else {
            None
        };
        match why {
            Some((reason, class)) => kept.push(Kept {
                name: s.name.clone(),
                reason,
                class,
            }),
            None => candidates.push(Candidate {
                name: s.name.clone(),
                idle_secs: age,
                windows: s.windows,
                command: if a.first_cmd.is_empty() {
                    "-".into()
                } else {
                    a.first_cmd.clone()
                },
            }),
        }
    }
    candidates.sort_by_key(|c| std::cmp::Reverse(c.idle_secs));
    StaleReport {
        candidates,
        kept,
        claude_checked: checked,
        claude_problem: problem,
    }
}

/// Every pane on the server, for [`classify`].
pub fn panes() -> Vec<PaneInfo> {
    let Ok(out) = tmux()
        .args([
            "list-panes",
            "-a",
            "-F",
            "#{pane_id}|#{pane_current_command}|#{pane_pid}|#{pane_in_mode}|#{pane_tty}|#{session_name}",
        ])
        .output()
    else {
        return Vec::new();
    };
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(|l| {
            let mut p = l.splitn(6, '|');
            Some(PaneInfo {
                pane_id: p.next()?.to_string(),
                command: p.next()?.to_string(),
                pid: p.next()?.trim().parse().unwrap_or(0),
                in_mode: p.next()?.trim() == "1",
                tty: p.next()?.trim_start_matches("/dev/").to_string(),
                session: p.next()?.to_string(),
            })
        })
        .collect()
}

/// `(pid, ppid, stat)` for every process.
pub fn procs() -> Vec<(i64, i64, String)> {
    let Ok(out) = Command::new("ps")
        .args(["-Ao", "pid=,ppid=,stat="])
        .output()
    else {
        return Vec::new();
    };
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(|l| {
            let mut it = l.split_whitespace();
            Some((
                it.next()?.parse().ok()?,
                it.next()?.parse().ok()?,
                it.next().unwrap_or("").to_string(),
            ))
        })
        .collect()
}

/// The shell pid of tmux pane `pane` (`%12`), while that pane exists.
pub fn pane_pid(pane: &str) -> Option<i64> {
    let out = tmux()
        .args(["display-message", "-p", "-t", pane, "#{pane_pid}"])
        .output()
        .ok()
        .filter(|o| o.status.success())?;
    String::from_utf8_lossy(&out.stdout).trim().parse().ok()
}

/// Pane `pane` still exists, is the one whose shell was `shell` (pane ids restart with the
/// tmux server), and that shell is running something — the command typed into it. A shell
/// back at its prompt (the command ended or never started) is not.
pub fn pane_running(pane: &str, shell: i64) -> bool {
    pane_pid(pane) == Some(shell) && procs().iter().any(|(_, ppid, _)| *ppid == shell)
}

/// Gather the live state for [`classify`]. `claude_check=false` skips the
/// cross-check (and says so in the report).
pub fn stale_input(claude_check: bool) -> Result<StaleInput> {
    let sessions = list_sessions()?;
    let claude = claude_check.then(|| {
        crate::core::discovery::discover_checked().map(|rows| {
            rows.iter()
                .map(|s| ClaudeRef {
                    tmux_backed: s.backend == crate::core::discovery::Backend::Tmux,
                    handle: s.handle.clone(),
                    tty: s
                        .tty
                        .as_deref()
                        .map(|t| t.trim_start_matches("/dev/").to_string()),
                    name: s.label(),
                    status: s.status.clone(),
                })
                .collect()
        })
    });
    Ok(StaleInput {
        now: chrono::Utc::now().timestamp(),
        current: current_session(),
        sessions,
        panes: panes(),
        procs: procs(),
        claude,
    })
}

/// What happened when one stale candidate was (not) killed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KillOutcome {
    Killed,
    Skipped(String),
}

/// Kill one candidate after re-checking, at the moment of the kill, everything
/// the report decided earlier: it still exists, nobody attached since, and no
/// pane has started a child process.
pub fn kill_if_still_stale(name: &str) -> KillOutcome {
    if !has_session(name) {
        return KillOutcome::Skipped("gone already".into());
    }
    // `-t =name:` pins display-message to the session (without the colon it's
    // read as a pane target and answers empty).
    let attached = tmux()
        .args([
            "display-message",
            "-p",
            "-t",
            &format!("={name}:"),
            "#{session_attached}",
        ])
        .output()
        .ok()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_default();
    if attached.is_empty() {
        return KillOutcome::Skipped("gone already".into());
    }
    if attached != "0" {
        return KillOutcome::Skipped("attached now".into());
    }
    let kids: HashSet<i64> = procs().into_iter().map(|(_, ppid, _)| ppid).collect();
    let busy = tmux()
        .args(["list-panes", "-s", "-t", &exact(name), "-F", "#{pane_pid}"])
        .output()
        .ok()
        .map(|o| {
            String::from_utf8_lossy(&o.stdout)
                .lines()
                .filter_map(|l| l.trim().parse::<i64>().ok())
                .any(|p| kids.contains(&p))
        })
        .unwrap_or(false);
    if busy {
        return KillOutcome::Skipped("started work".into());
    }
    match kill_session(name) {
        Ok(()) => KillOutcome::Killed,
        Err(e) => KillOutcome::Skipped(format!("could not kill: {e}")),
    }
}

// --- one session per pane ----------------------------------------------------

/// A tmux session by its id (`$3`), which the server never gives to another session, and
/// the name it had when it was read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostSession {
    pub id: String,
    pub name: String,
}

/// How pane `pane` sits in its tmux session, read now.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Hosting {
    /// The pane is the only pane of the only window of this tmux session, and that window is
    /// linked into no other session.
    Alone(HostSession),
    /// Tmux session `session` holds more than this pane: `windows` windows, `panes` panes —
    /// or its one window is also linked into another session (`linked`).
    Shared {
        session: String,
        windows: usize,
        panes: usize,
        linked: bool,
    },
    /// Confirmed: the server has no pane `pane` (or there is no server).
    Gone,
}

/// Where pane `pane` (`%12`) lives: alone in its tmux session — one window, one pane, this
/// one — or beside others. One `list-panes -a`: a single snapshot of the whole server, the
/// session found by its id, so nothing can be renamed or moved between two reads. `Err` (as
/// `io`) when tmux could not be asked (anything but "no server"): a failed query is never
/// taken for the pane being gone.
pub fn hosting(pane: &str) -> Result<Hosting> {
    let listing = query(&[
        "list-panes",
        "-a",
        "-F",
        "#{session_id}\t#{window_id}\t#{window_linked}\t#{pane_id}\t#{session_name}",
    ])?;
    Ok(listing.map_or(Hosting::Gone, |l| hosting_of(pane, &l)))
}

/// [`hosting`] from its `list-panes -a` listing.
fn hosting_of(pane: &str, listing: &str) -> Hosting {
    let rows: Vec<Vec<&str>> = listing
        .lines()
        .map(|l| l.splitn(5, '\t').collect::<Vec<_>>())
        .filter(|r| r.len() == 5)
        .collect();
    let Some(own) = rows.iter().find(|r| r[3] == pane) else {
        return Hosting::Gone;
    };
    let mine: Vec<&Vec<&str>> = rows.iter().filter(|r| r[0] == own[0]).collect();
    let windows: HashSet<&str> = mine.iter().map(|r| r[1]).collect();
    let linked = own[2] != "0";
    if mine.len() == 1 && !linked {
        Hosting::Alone(HostSession {
            id: own[0].to_string(),
            name: own[4].to_string(),
        })
    } else {
        Hosting::Shared {
            session: own[4].to_string(),
            windows: windows.len(),
            panes: mine.len(),
            linked,
        }
    }
}

/// What [`kill_if_alone`] found, in the same instant it killed or did not.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KillAlone {
    /// The tmux session was killed, and is confirmed gone; `.0` is its name at the kill.
    Killed(String),
    /// Not killed: the session (still there) no longer holds this pane alone.
    Refused {
        windows: usize,
        panes: usize,
        linked: bool,
        /// The pane checked was this one (else this pane is no longer in it).
        ours: bool,
    },
    /// Confirmed: the session no longer exists (or there is no server); nothing was killed.
    Gone,
}

/// Kill tmux session `session` (its id, `$3`) only if, at that very instant, pane `pane`
/// (`%12`) is the one pane of its one window, linked nowhere else. The check and the kill are
/// one tmux command — `if-shell -F`, whose format is evaluated and whose kill runs in the same
/// pass over this client's command queue, before the server reads any other client — so
/// nothing can be renamed, replaced, split or linked between them; and ids, unlike names, are
/// never reused. The outcome is then confirmed against the session list: `Killed` only once
/// the id is gone from it. `Err` (as `io`) when tmux could not be asked, or answered
/// something else.
pub fn kill_if_alone(session: &str, pane: &str) -> Result<KillAlone> {
    let is_id = |s: &str, sigil: char| {
        s.strip_prefix(sigil)
            .is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
    };
    if !is_id(session, '$') || !is_id(pane, '%') {
        return Err(Error::tagged(
            "io",
            format!("not a tmux session / pane id: {session} / {pane}"),
        ));
    }
    let out = tmux()
        .args([
            "if-shell",
            "-F",
            "-t",
            session,
            &alone_format(session, pane),
            // The ids are digits behind a sigil, so they quote safely; the name is only
            // expanded when printed, never parsed.
            &format!(
                "display-message -p -t '{session}' 'killed\t#{{session_name}}' ; kill-session -t '{session}'"
            ),
            &format!(
                "display-message -p -t '{session}' 'refused\t#{{session_id}}\t#{{session_windows}}\t#{{window_panes}}\t#{{window_linked}}\t#{{pane_id}}'"
            ),
        ])
        .output()
        .map_err(|e| Error::tagged("io", format!("tmux: {e}")))?;
    let stdout = String::from_utf8_lossy(&out.stdout).to_string();
    let stderr = String::from_utf8_lossy(&out.stderr).trim().to_string();
    if !out.status.success() {
        if no_server(&stderr) {
            return Ok(KillAlone::Gone);
        }
        return Err(Error::tagged(
            "io",
            format!("tmux if-shell: {}", or_status(&stderr, out.status)),
        ));
    }
    let exists = session_exists(session)?;
    let f: Vec<&str> = stdout.lines().next().unwrap_or("").split('\t').collect();
    match f[0] {
        "killed" if !exists => Ok(KillAlone::Killed(f[1..].join("\t"))),
        "killed" => Err(Error::tagged(
            "io",
            format!("tmux session {session} was killed but is still listed"),
        )),
        // No such session: `if-shell` fell back to another one, which the format (its
        // session id) refused.
        _ if !exists => Ok(KillAlone::Gone),
        "refused" if f.len() == 6 && f[1] == session => Ok(KillAlone::Refused {
            windows: f[2].parse().unwrap_or(0),
            panes: f[3].parse().unwrap_or(0),
            linked: f[4] != "0",
            ours: f[5] == pane,
        }),
        _ => Err(Error::tagged(
            "io",
            format!("tmux if-shell: unexpected answer {:?}", stdout.trim()),
        )),
    }
}

/// The `if-shell -F` condition of [`kill_if_alone`]: true only for session `session` holding
/// one window, unlinked, holding one pane, `pane`.
fn alone_format(session: &str, pane: &str) -> String {
    let eq = |var: &str, v: &str| format!("#{{==:#{{{var}}},{v}}}");
    [
        eq("session_windows", "1"),
        eq("window_panes", "1"),
        eq("window_linked", "0"),
        eq("pane_id", pane),
    ]
    .into_iter()
    .fold(eq("session_id", session), |all, c| {
        format!("#{{&&:{all},{c}}}")
    })
}

/// Does tmux session `id` (`$3`) exist? `false` with no server at all; `Err` (as `io`) when
/// tmux could not be asked.
pub fn session_exists(id: &str) -> Result<bool> {
    Ok(query(&["list-sessions", "-F", "#{session_id}"])?
        .is_some_and(|l| l.lines().any(|s| s.trim() == id)))
}

/// The terminal of pane `pane` (`/dev/ttys012`); `None` when the server has no such pane.
pub fn pane_tty(pane: &str) -> Result<Option<String>> {
    Ok(
        query(&["list-panes", "-a", "-F", "#{pane_id}\t#{pane_tty}"])?.and_then(|l| {
            l.lines()
                .filter_map(|r| r.split_once('\t'))
                .find(|(p, _)| *p == pane)
                .map(|(_, t)| t.to_string())
        }),
    )
}

/// A read-only tmux query's stdout; `None` when no server is running (so nothing it asks
/// about exists), `Err` (as `io`) for any other failure.
fn query(args: &[&str]) -> Result<Option<String>> {
    let out = tmux()
        .args(args)
        .output()
        .map_err(|e| Error::tagged("io", format!("tmux: {e}")))?;
    let stderr = String::from_utf8_lossy(&out.stderr).trim().to_string();
    if out.status.success() {
        Ok(Some(String::from_utf8_lossy(&out.stdout).to_string()))
    } else if no_server(&stderr) {
        Ok(None)
    } else {
        Err(Error::tagged(
            "io",
            format!(
                "tmux {}: {}",
                args.first().unwrap_or(&""),
                or_status(&stderr, out.status)
            ),
        ))
    }
}

/// Is this tmux's "no server" (nothing to connect to), as opposed to a failure?
fn no_server(stderr: &str) -> bool {
    stderr.starts_with("no server running on ")
        || (stderr.starts_with("error connecting to ") && stderr.contains("No such file"))
}

fn or_status(stderr: &str, status: std::process::ExitStatus) -> String {
    if stderr.is_empty() {
        format!("exited {status}")
    } else {
        stderr.to_string()
    }
}

/// Is process `pid` still running? A zombie (exited, not yet reaped) is not.
pub fn process_alive(pid: i64) -> bool {
    Command::new("ps")
        .args(["-o", "stat=", "-p", &pid.to_string()])
        .output()
        .ok()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .is_some_and(|stat| !stat.is_empty() && !stat.starts_with('Z'))
}

#[cfg(test)]
mod tests {
    use super::*;

    // `fleet tmux new` hands its panes the PATH-only environment with tmux `-e`, before the
    // command (which tmux takes as the rest of its arguments).
    #[test]
    fn a_new_session_starts_with_the_pane_env() {
        let env = ["PATH=/t/bin".to_string(), "FLEET_PATH_ONLY=1".into()];
        assert_eq!(
            new_session_args("w", Some("/tmp"), &env, &["top".into()]),
            [
                "new-session",
                "-d",
                "-s",
                "w",
                "-c",
                "/tmp",
                "-e",
                "PATH=/t/bin",
                "-e",
                "FLEET_PATH_ONLY=1",
                "top"
            ]
        );
        assert_eq!(
            new_session_args("w", None, &[], &[]),
            ["new-session", "-d", "-s", "w"]
        );
    }

    fn sess(name: &str, last: i64, att: i64) -> TmuxSession {
        TmuxSession {
            name: name.into(),
            attached: att,
            windows: 1,
            last_attached: last,
            activity: last,
            created: last,
        }
    }

    #[test]
    fn parses_list_sessions_lines() {
        let s = parse_session("100|1|3|200|50|my|odd|name").unwrap();
        assert_eq!(s.name, "my|odd|name");
        assert_eq!(
            (
                s.last_attached,
                s.attached,
                s.windows,
                s.activity,
                s.created
            ),
            (100, 1, 3, 200, 50)
        );
        assert!(parse_session("1|2|3").is_none());
        assert!(parse_session("1|0|1|1|1|").is_none());
    }

    #[test]
    fn sorted_by_last_attached_then_activity() {
        let mut v = vec![sess("a", 10, 0), sess("b", 30, 0), sess("c", 20, 0)];
        v[0].activity = 99;
        sort_sessions(&mut v);
        let names: Vec<_> = v.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, ["b", "c", "a"]);
    }

    #[test]
    fn matching_tiers() {
        let v = vec![
            sess("api", 1, 0),
            sess("API-2", 1, 0),
            sess("frontend", 1, 0),
            sess("my-api-x", 1, 0),
        ];
        assert_eq!(resolve_in(&v, "api"), Resolve::One("api".into()));
        assert_eq!(resolve_in(&v, "Api"), Resolve::One("api".into()));
        assert_eq!(resolve_in(&v, "fr"), Resolve::One("frontend".into()));
        assert_eq!(resolve_in(&v, "api-"), Resolve::One("API-2".into()));
        assert_eq!(resolve_in(&v, "-x"), Resolve::One("my-api-x".into()));
        assert_eq!(resolve_in(&v, "zzz"), Resolve::None);
        assert_eq!(resolve_in(&v, ""), Resolve::None);
        match resolve_in(&v, "a") {
            Resolve::Many(m) => assert_eq!(m, ["api", "API-2"]),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn last_skips_the_current_session() {
        let v = vec![sess("here", 30, 1), sess("before", 20, 0)];
        assert_eq!(last_target(&v, Some("here")).as_deref(), Some("before"));
        assert_eq!(last_target(&v, None).as_deref(), Some("here"));
        assert_eq!(last_target(&v[..1], Some("here")), None);
    }

    #[test]
    fn names_are_sanitized() {
        assert_eq!(sanitize_name("my session"), "my-session");
        assert_eq!(sanitize_name("a:b.c"), "a-b-c");
        assert_eq!(sanitize_name("--x--"), "x");
        assert_eq!(sanitize_name("two\nlines\ttab"), "two-lines-tab");
        assert_eq!(sanitize_name("čćž"), "");
        assert_eq!(sanitize_name("ok_name-1"), "ok_name-1");
    }

    #[test]
    fn spans() {
        assert_eq!(parse_span("30m"), Ok(1800));
        assert_eq!(parse_span("12h"), Ok(43200));
        assert_eq!(parse_span("7d"), Ok(604_800));
        assert_eq!(parse_span("24"), Ok(86400));
        assert_eq!(parse_span("010h"), Ok(36000), "decimal, not octal");
        assert_eq!(parse_span("0"), Ok(0));
        assert!(parse_span("").is_err());
        assert!(parse_span("5w").is_err());
        assert!(parse_span("-5h").is_err());
        assert!(parse_span("3651d").unwrap_err().contains("out of range"));
        assert!(parse_span("99999999999999999999").is_err());
        assert_eq!(dur(45), "45s");
        assert_eq!(dur(3 * 3600), "3h");
        assert_eq!(dur(2 * 86400 + 5), "2d");
    }

    fn pane(id: &str, cmd: &str, pid: i64, tty: &str, s: &str) -> PaneInfo {
        PaneInfo {
            pane_id: id.into(),
            command: cmd.into(),
            pid,
            in_mode: false,
            tty: tty.into(),
            session: s.into(),
        }
    }

    fn base() -> StaleInput {
        let now = 1_000_000;
        let old = now - 3 * 86400;
        let mut sessions = vec![
            sess("idle-old", old, 0),
            sess("here", old, 0),
            sess("attached", old, 1),
            sess("fleet", old, 0),
            sess("claude-in", old, 0),
            sess("vim", old, 0),
            sess("bg", old, 0),
            sess("recent", now - 60, 0),
            sess("older", old - 86400, 0),
        ];
        sessions[5].windows = 2;
        StaleInput {
            now,
            current: Some("here".into()),
            sessions,
            panes: vec![
                pane("%1", "zsh", 101, "ttys001", "idle-old"),
                pane("%2", "zsh", 102, "ttys002", "here"),
                pane("%3", "zsh", 103, "ttys003", "attached"),
                pane("%4", "tb", 104, "ttys004", "fleet"),
                pane("%5", "zsh", 105, "ttys005", "claude-in"),
                pane("%6", "nvim", 106, "ttys006", "vim"),
                pane("%7", "zsh", 107, "ttys007", "bg"),
                pane("%8", "zsh", 108, "ttys008", "recent"),
                pane("%9", "-zsh", 109, "ttys009", "older"),
            ],
            procs: vec![(500, 107, "S".into())],
            claude: Some(Ok(vec![ClaudeRef {
                tmux_backed: true,
                handle: Some("%5".into()),
                tty: Some("ttys005".into()),
                name: "app-1".into(),
                status: "idle".into(),
            }])),
        }
    }

    #[test]
    fn classify_keeps_everything_in_use() {
        let r = classify(&base(), 86400);
        assert!(r.claude_checked, "{:?}", r.claude_problem);
        let cands: Vec<_> = r.candidates.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(cands, ["older", "idle-old"], "idle-longest first");
        let why = |n: &str| {
            r.kept
                .iter()
                .find(|k| k.name == n)
                .map(|k| (k.class, k.reason.clone()))
                .unwrap()
        };
        assert_eq!(why("here").0, KeptClass::Here);
        assert_eq!(why("attached").0, KeptClass::Attached);
        assert_eq!(why("fleet").0, KeptClass::Infra);
        assert_eq!(
            why("claude-in"),
            (KeptClass::Claude, "claude: app-1 idle".into())
        );
        assert_eq!(why("vim"), (KeptClass::Running, "running: nvim".into()));
        assert_eq!(why("bg"), (KeptClass::Running, "background job".into()));
        assert_eq!(why("recent").0, KeptClass::Recent);
    }

    #[test]
    fn a_stale_claude_map_fails_the_check() {
        let mut i = base();
        i.claude = Some(Ok(vec![ClaudeRef {
            tmux_backed: true,
            handle: Some("%5".into()),
            tty: Some("ttys001".into()),
            name: "x".into(),
            status: "busy".into(),
        }]));
        let r = classify(&i, 86400);
        assert!(!r.claude_checked);
        assert!(r.claude_problem.unwrap().contains("disagreement"));
        // Both sessions it could be in are protected anyway.
        assert!(r.candidates.iter().all(|c| c.name != "idle-old"));

        let mut i = base();
        i.claude = Some(Ok(vec![ClaudeRef {
            tmux_backed: true,
            handle: Some("%77".into()),
            tty: Some("ttys077".into()),
            ..Default::default()
        }]));
        let r = classify(&i, 86400);
        assert!(!r.claude_checked);
        assert!(r.claude_problem.unwrap().contains("no live pane"));
    }

    #[test]
    fn skipped_or_failed_checks_are_reported() {
        let mut i = base();
        i.claude = None;
        let r = classify(&i, 86400);
        assert!(!r.claude_checked);
        // Without the check, the Claude session is just another idle shell.
        assert!(r.candidates.iter().any(|c| c.name == "claude-in"));

        i.claude = Some(Err("registry unreadable".into()));
        let r = classify(&i, 86400);
        assert_eq!(r.claude_problem.as_deref(), Some("registry unreadable"));
    }

    #[test]
    fn non_tmux_claude_sessions_are_protected_by_tty() {
        let mut i = base();
        i.claude = Some(Ok(vec![
            ClaudeRef {
                tmux_backed: false,
                handle: None,
                tty: Some("ttys001".into()),
                name: "odd".into(),
                status: "idle".into(),
            },
            ClaudeRef {
                tmux_backed: false,
                handle: Some("GUID".into()),
                tty: Some("ttys099".into()),
                name: "iterm".into(),
                status: "idle".into(),
            },
        ]));
        let r = classify(&i, 86400);
        assert!(r.claude_checked, "an iTerm session is not a stale map");
        assert!(r.candidates.iter().all(|c| c.name != "idle-old"));
    }

    #[test]
    fn a_pane_is_alone_only_as_the_one_pane_of_the_one_unlinked_window() {
        let alone = |id: &str, name: &str| {
            Hosting::Alone(HostSession {
                id: id.into(),
                name: name.into(),
            })
        };
        let other = "$1\t@9\t0\t%9\twork\n";
        assert_eq!(
            hosting_of("%3", &format!("$4\t@1\t0\t%3\tjob\n{other}")),
            alone("$4", "job")
        );
        // The session is found by its id: another one with the same name is not it.
        assert_eq!(
            hosting_of("%3", "$4\t@1\t0\t%3\tjob\n$5\t@2\t0\t%5\tjob\n"),
            alone("$4", "job")
        );
        // A name with a tab or spaces is the whole last field.
        assert_eq!(
            hosting_of("%3", "$4\t@1\t0\t%3\tmy\tjob x\n"),
            alone("$4", "my\tjob x")
        );
        assert_eq!(
            hosting_of("%3", "$1\t@1\t0\t%3\twork\n$1\t@2\t0\t%4\twork\n"),
            Hosting::Shared {
                session: "work".into(),
                windows: 2,
                panes: 2,
                linked: false
            }
        );
        assert_eq!(
            hosting_of("%3", "$4\t@1\t0\t%3\tjob\n$4\t@1\t0\t%4\tjob\n"),
            Hosting::Shared {
                session: "job".into(),
                windows: 1,
                panes: 2,
                linked: false
            }
        );
        // Its one window is linked into another session too: killing this one would not
        // end it.
        assert_eq!(
            hosting_of("%3", "$4\t@1\t1\t%3\tjob\n$1\t@1\t1\t%3\twork\n"),
            Hosting::Shared {
                session: "job".into(),
                windows: 1,
                panes: 1,
                linked: true
            }
        );
        assert_eq!(hosting_of("%3", other), Hosting::Gone);
        assert_eq!(hosting_of("%3", ""), Hosting::Gone);
    }

    #[test]
    fn the_kill_condition_checks_every_identity_at_once() {
        assert_eq!(
            alone_format("$4", "%3"),
            "#{&&:#{&&:#{&&:#{&&:#{==:#{session_id},$4},#{==:#{session_windows},1}},\
             #{==:#{window_panes},1}},#{==:#{window_linked},0}},#{==:#{pane_id},%3}}"
        );
    }

    #[test]
    fn only_tmux_ids_are_put_in_a_kill() {
        for (s, p) in [("job", "%3"), ("$4", "3"), ("$4'", "%3"), ("$", "%3")] {
            let e = kill_if_alone(s, p).unwrap_err();
            assert_eq!(e.kind(), "io", "{s} {p}");
        }
    }

    #[test]
    fn no_server_is_nothing_there_not_a_failure() {
        // The unit tests' private socket has no server.
        assert_eq!(hosting("%3").unwrap(), Hosting::Gone);
        assert!(!session_exists("$4").unwrap());
        assert_eq!(pane_tty("%3").unwrap(), None);
        assert_eq!(kill_if_alone("$4", "%3").unwrap(), KillAlone::Gone);
        assert!(no_server("no server running on /tmp/x/default"));
        assert!(!no_server("can't find session: $4"));
    }

    #[test]
    fn this_process_is_alive_and_a_bogus_pid_is_not() {
        assert!(process_alive(std::process::id() as i64));
        assert!(!process_alive(999_999_999));
    }
}
