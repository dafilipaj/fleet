//! Session recovery (docs/architecture.md → "Session recovery").
//!
//! A reboot kills the tmux server and every `claude` process, and with them everything fleet
//! knows about a machine's fleet. This module keeps one small state file per machine — the
//! **snapshot** (`$FLEET_SNAPSHOT`, else `${XDG_STATE_HOME:-~/.local/state}/fleet/snapshot.json`)
//! — recording what was running: every tmux session's windows, panes, cwds and layout, and
//! which pane holds which Claude session (with the flags it was started with). Claude sessions
//! outside tmux (iTerm, unknown) are recorded on their own.
//!
//! [`record`] runs on every local discovery that already has fresh rows (`fleet list`). When it
//! sees a new **boot id**, whatever the old boot was running becomes **dormant**; [`restore`]
//! brings a dormant entry back: the same tmux session name, windows, panes and cwds, and each
//! Claude pane re-launched with `<launcher> <flags> --resume=<sessionId>`. The session id stays
//! the same (only `--fork-session` mints a new one), so groups, stacks and briefs carry over.
//!
//! Within one boot a session that disappears was closed on purpose and is *not* dormant —
//! except in the last [`GRACE_SECS`] before a reboot: a restart quits the terminal apps (and
//! the Claude sessions in them) before it kills the daemons, and a `fleet list` polled in
//! between must not erase them. So a vanished entry lingers with `goneAt` for the grace
//! period, and only then moves into the **closed** list (`closed`, kept for
//! `restore.keepClosedDays`, newest [`MAX_CLOSED`]). Closed entries — lingering ones
//! included, see [`closed_entries`] — can be brought back with [`restore_closed`], but never
//! count as dormant: no "restore all", no restore on boot, no group/stack membership.
//!
//! Nothing in here prints. The file is a contract like `groups.json`: unknown keys survive a
//! rewrite, at every level.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::core::discovery::{self, Backend, Session};
use crate::core::harness::Harness;
use crate::core::tools;
use crate::error::{Error, Result};

pub const VERSION: u64 = 1;
/// Exit code: a target matches several dormant sessions.
pub const EXIT_AMBIGUOUS: i32 = 2;
/// Exit code: no dormant session matches.
pub const EXIT_NOTHING: i32 = 3;
/// How long a vanished session lingers (`goneAt`) before it counts as closed on purpose.
pub const GRACE_SECS: i64 = 300;
/// An unchanged snapshot is still rewritten this often, so `updatedAt` — which becomes the
/// dormant entries' `since` — stays close to the moment the machine went down.
pub const REFRESH_SECS: i64 = 600;
/// `restore.keepClosedDays` when the config doesn't say.
pub const DEFAULT_KEEP_CLOSED_DAYS: f64 = 7.0;
/// At most this many closed entries are kept (the newest).
pub const MAX_CLOSED: usize = 50;
/// Shortest session-id prefix a restore target may be.
const MIN_ID_PREFIX: usize = 4;
/// Flags replayed on resume without a value.
const BOOL_FLAGS: [&str; 2] = ["--dangerously-skip-permissions", "--chrome"];
/// Flags replayed on resume with their value. `--add-dir` may repeat.
const VALUE_FLAGS: [&str; 5] = [
    "--model",
    "--permission-mode",
    "--add-dir",
    "--agent",
    "--fallback-model",
];

// ------------------------------------------------------------------ the file

/// One Claude session, as much as resuming it needs.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", default)]
pub struct ClaudeSnap {
    pub session_id: String,
    pub name: Option<String>,
    pub cwd: Option<String>,
    /// The display title when it was recorded — what every view called it.
    pub title: Option<String>,
    /// The replay-safe part of its command line (see [`replayable_flags`]).
    pub flags: Vec<String>,
    /// Dormant entries only: when it went down (≈ the old boot's last record).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub since: Option<String>,
    /// Live entries only: when it vanished, within [`GRACE_SECS`].
    #[serde(skip_serializing_if = "Option::is_none")]
    pub gone_at: Option<String>,
    /// Closed entries only: when it ended (within a boot — closed on purpose).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub closed_at: Option<String>,
    /// The tmux session it ran in, kept when it leaves its pane (claude exited, the pane
    /// lived on): a closed one is resumed in a new window there while that session exists.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tmux_session: Option<String>,
    /// Which harness runs it; absent (older files, every Claude session) = Claude.
    #[serde(skip_serializing_if = "is_claude")]
    pub harness: Harness,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

fn is_claude(h: &Harness) -> bool {
    *h == Harness::Claude
}

impl ClaudeSnap {
    /// What a view calls it: the recorded title, else the name, else the short id.
    pub fn label(&self) -> String {
        self.title
            .clone()
            .or_else(|| self.name.clone())
            .filter(|t| !t.trim().is_empty())
            .unwrap_or_else(|| self.session_id.chars().take(8).collect())
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", default)]
pub struct PaneSnap {
    pub index: i64,
    pub cwd: String,
    pub active: bool,
    pub claude: Option<ClaudeSnap>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", default)]
pub struct WindowSnap {
    pub index: i64,
    pub name: String,
    /// `#{window_layout}`, replayed with `select-layout`.
    pub layout: String,
    pub active: bool,
    /// tmux named the window itself (`automatic-rename` on): a restore leaves the name to
    /// tmux again instead of freezing whatever command was running.
    pub auto_name: bool,
    pub panes: Vec<PaneSnap>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", default)]
pub struct TmuxSnap {
    pub name: String,
    pub windows: Vec<WindowSnap>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub since: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub gone_at: Option<String>,
    /// Closed entries only: when it ended.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub closed_at: Option<String>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

impl TmuxSnap {
    /// Every Claude session in it, in window/pane order.
    pub fn claudes(&self) -> impl Iterator<Item = &ClaudeSnap> {
        self.windows
            .iter()
            .flat_map(|w| w.panes.iter())
            .filter_map(|p| p.claude.as_ref())
    }

    pub fn pane_count(&self) -> usize {
        self.windows.iter().map(|w| w.panes.len()).sum()
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", default)]
pub struct Dormant {
    pub tmux: Vec<TmuxSnap>,
    pub iterm: Vec<ClaudeSnap>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

impl Dormant {
    pub fn is_empty(&self) -> bool {
        self.tmux.is_empty() && self.iterm.is_empty()
    }

    pub fn len(&self) -> usize {
        self.tmux.len() + self.iterm.len()
    }
}

/// Sessions that ended within a boot — closed on purpose (Close in the app, `fleet kill`,
/// `/exit`, a killed tmux session). Never dormant: nothing restores them unasked.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", default)]
pub struct Closed {
    /// Whole tmux sessions (that had a Claude pane), with their layout.
    pub tmux: Vec<TmuxSnap>,
    /// Lone Claude sessions: not in tmux, or gone from a tmux pane that lived on.
    pub claude: Vec<ClaudeSnap>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

impl Closed {
    pub fn is_empty(&self) -> bool {
        self.tmux.is_empty() && self.claude.is_empty()
    }

    pub fn len(&self) -> usize {
        self.tmux.len() + self.claude.len()
    }

    /// Every session id in it.
    fn ids(&self) -> HashSet<String> {
        self.tmux
            .iter()
            .flat_map(TmuxSnap::claudes)
            .chain(self.claude.iter())
            .map(|c| c.session_id.clone())
            .collect()
    }

    /// Take `ids` out: lone entries go, tmux panes become plain shells, and a tmux entry
    /// left without a Claude pane goes.
    fn drop_ids(&mut self, ids: &HashSet<String>) {
        self.claude.retain(|c| !ids.contains(&c.session_id));
        for t in &mut self.tmux {
            for p in t.windows.iter_mut().flat_map(|w| w.panes.iter_mut()) {
                if p.claude
                    .as_ref()
                    .is_some_and(|c| ids.contains(&c.session_id))
                {
                    p.claude = None;
                }
            }
        }
        self.tmux.retain(|t| t.claudes().next().is_some());
    }

    /// Add a whole tmux session that just closed (newer than everything here): its ids leave
    /// older entries, and an older closed session of the same name hands what Claude panes
    /// it has left over as lone sessions (hinted with the name).
    fn add_tmux(&mut self, mut t: TmuxSnap) {
        for p in t.windows.iter_mut().flat_map(|w| w.panes.iter_mut()) {
            if let Some(c) = p.claude.as_mut() {
                c.gone_at = None;
                c.since = None;
            }
        }
        if t.claudes().next().is_none() {
            return;
        }
        let ids: HashSet<String> = t.claudes().map(|c| c.session_id.clone()).collect();
        self.drop_ids(&ids);
        if let Some(i) = self.tmux.iter().position(|o| o.name == t.name) {
            let old = self.tmux.remove(i);
            for c in old.claudes() {
                self.claude.push(ClaudeSnap {
                    closed_at: old.closed_at.clone(),
                    tmux_session: Some(old.name.clone()),
                    ..c.clone()
                });
            }
        }
        self.tmux.push(t);
    }

    /// Add a lone Claude session that just closed (newer than everything here).
    fn add_claude(&mut self, c: ClaudeSnap) {
        self.drop_ids(&HashSet::from([c.session_id.clone()]));
        self.claude.push(ClaudeSnap {
            gone_at: None,
            since: None,
            ..c
        });
    }

    /// Drop what is older than `keep` seconds (or has no readable `closedAt`), then keep the
    /// newest [`MAX_CLOSED`].
    fn expire(&mut self, keep: i64, now: DateTime<Utc>) {
        let fresh = |at: &Option<String>| {
            at.as_deref()
                .and_then(|a| age(a, now))
                .is_some_and(|a| a <= keep)
        };
        self.tmux.retain(|t| fresh(&t.closed_at));
        self.claude.retain(|c| fresh(&c.closed_at));
        if self.len() <= MAX_CLOSED {
            return;
        }
        let mut stamps: Vec<String> = self
            .tmux
            .iter()
            .map(|t| t.closed_at.clone().unwrap_or_default())
            .chain(
                self.claude
                    .iter()
                    .map(|c| c.closed_at.clone().unwrap_or_default()),
            )
            .collect();
        stamps.sort_unstable_by(|a, b| b.cmp(a));
        let cut = stamps[MAX_CLOSED - 1].clone();
        // Ties at the cut: the earliest-listed survive, up to the cap.
        let mut room = MAX_CLOSED - stamps.iter().filter(|s| **s > cut).count();
        let mut keep_one = |at: &Option<String>| {
            let at = at.clone().unwrap_or_default();
            if at > cut {
                return true;
            }
            if at == cut && room > 0 {
                room -= 1;
                return true;
            }
            false
        };
        self.tmux.retain(|t| keep_one(&t.closed_at));
        self.claude.retain(|c| keep_one(&c.closed_at));
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", default)]
pub struct Snapshot {
    pub version: u64,
    pub host: String,
    /// `None` = unknown, and then nothing is ever marked dormant.
    pub boot_id: Option<String>,
    pub updated_at: Option<String>,
    /// Live tmux sessions of the current boot.
    pub tmux: Vec<TmuxSnap>,
    /// Live Claude sessions of the current boot that are not in a tmux pane.
    pub iterm: Vec<ClaudeSnap>,
    pub dormant: Dormant,
    /// What ended within a boot, for [`keep_closed_secs`] (see [`Closed`]).
    pub closed: Closed,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

impl Snapshot {
    /// Equal apart from `updatedAt` — whether a record has anything to write.
    fn same_content(&self, other: &Snapshot) -> bool {
        let strip = |s: &Snapshot| Snapshot {
            updated_at: None,
            ..s.clone()
        };
        strip(self) == strip(other)
    }
}

/// `$FLEET_SNAPSHOT`, else `${XDG_STATE_HOME:-~/.local/state}/fleet/snapshot.json`.
pub fn path() -> PathBuf {
    if let Some(p) = std::env::var_os("FLEET_SNAPSHOT").filter(|p| !p.is_empty()) {
        return PathBuf::from(tools::expand_tilde(&p.to_string_lossy()));
    }
    let base = std::env::var_os("XDG_STATE_HOME")
        .filter(|p| !p.is_empty())
        .map(|p| PathBuf::from(tools::expand_tilde(&p.to_string_lossy())))
        .unwrap_or_else(|| {
            dirs::home_dir()
                .unwrap_or_default()
                .join(".local")
                .join("state")
        });
    base.join("fleet").join("snapshot.json")
}

/// The snapshot at `path`; a missing file is an empty one, an unparsable one an error.
pub fn load_from(path: &Path) -> Result<Snapshot> {
    match std::fs::read_to_string(path) {
        Ok(t) => serde_json::from_str(&t)
            .map_err(|e| Error::Other(format!("{} is not a valid snapshot: {e}", path.display()))),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Snapshot::default()),
        Err(e) => Err(e.into()),
    }
}

/// Atomic, `0600` in a `0700` dir (the briefs' writer).
pub fn save_to(path: &Path, snap: &Snapshot) -> Result<()> {
    crate::core::brief::write_private(path, &serde_json::to_string_pretty(snap)?)
}

/// An exclusive lock on `<path>.lock` for one read-modify-write. `record` runs on every
/// `fleet list` (the web server polls it), so without this a restore's "no longer dormant"
/// could be overwritten by a poll that read the file a moment earlier.
fn lock(path: &Path) -> Result<std::fs::File> {
    use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
    if let Some(dir) = path.parent() {
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(dir)?;
    }
    let mut name = path.as_os_str().to_owned();
    name.push(".lock");
    let f = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .mode(0o600)
        .open(PathBuf::from(name))?;
    f.lock()?;
    Ok(f)
}

/// Load, change, save — under the lock. `f` says whether it changed anything.
fn update<T>(f: impl FnOnce(&mut Snapshot) -> Result<(bool, T)>) -> Result<T> {
    let p = path();
    let _guard = lock(&p)?;
    let mut snap = load_from(&p)?;
    let (changed, out) = f(&mut snap)?;
    if changed {
        snap.version = VERSION;
        snap.updated_at = Some(iso(Utc::now()));
        save_to(&p, &snap)?;
    }
    Ok(out)
}

fn iso(t: DateTime<Utc>) -> String {
    t.to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

/// Seconds from `iso` to `now`; `None` when it doesn't parse.
fn age(iso: &str, now: DateTime<Utc>) -> Option<i64> {
    DateTime::parse_from_rfc3339(iso)
        .ok()
        .map(|t| (now - t.with_timezone(&Utc)).num_seconds())
}

/// The host name entries are recorded under: `--as-host`, else config `self`.
fn host_name() -> String {
    crate::core::hosts::as_host().unwrap_or_else(|| crate::core::config::get().self_name())
}

// ------------------------------------------------------------------ boot id

/// This boot's identity: `$FLEET_BOOT_ID` (tests, a simulated reboot), else Linux's
/// `boot_id`, else macOS `kern.boottime`'s seconds. `None` when none can be read.
pub fn boot_id() -> Option<String> {
    if let Ok(v) = std::env::var("FLEET_BOOT_ID") {
        let v = v.trim();
        if !v.is_empty() {
            return Some(v.to_string());
        }
    }
    if let Ok(t) = std::fs::read_to_string("/proc/sys/kernel/random/boot_id") {
        let t = t.trim();
        if !t.is_empty() {
            return Some(t.to_string());
        }
    }
    let out = std::process::Command::new("sysctl")
        .args(["-n", "kern.boottime"])
        .output()
        .ok()?;
    parse_boottime(&String::from_utf8_lossy(&out.stdout))
}

/// `{ sec = 1727000000, usec = 315387 } Tue Sep …` → `1727000000`.
fn parse_boottime(s: &str) -> Option<String> {
    let rest = &s[s.find("sec =")? + "sec =".len()..];
    let n: String = rest
        .trim_start()
        .chars()
        .take_while(char::is_ascii_digit)
        .collect();
    (!n.is_empty()).then_some(n)
}

// ------------------------------------------------------------------ flags

/// The replay-safe part of a `claude` command line (argv without argv\[0\]), normalised to
/// `--flag` / `--flag value`: `--dangerously-skip-permissions`, `--chrome`, `--model`,
/// `--permission-mode`, `--add-dir` (repeatable, one value each), `--agent`,
/// `--fallback-model`. Everything else — the prompt, `-r/--resume`, `-c/--continue`,
/// `-n/--name`, `-p`, `--session-id`, file paths — would change what a resume does, so it
/// is dropped. Both `--k v` and `--k=v` parse; a repeated non-repeatable flag keeps its last
/// value, which is the one claude used.
pub fn replayable_flags(argv: &[String]) -> Vec<String> {
    let mut groups: Vec<Vec<String>> = Vec::new();
    let mut push = |g: Vec<String>| {
        let repeatable = g[0] == "--add-dir";
        if repeatable {
            if !groups.contains(&g) {
                groups.push(g);
            }
        } else {
            groups.retain(|x| x[0] != g[0]);
            groups.push(g);
        }
    };
    let mut i = 0;
    while i < argv.len() {
        let a = argv[i].as_str();
        i += 1;
        if BOOL_FLAGS.contains(&a) {
            push(vec![a.to_string()]);
        } else if let Some((k, v)) = a.split_once('=')
            && VALUE_FLAGS.contains(&k)
        {
            if !v.is_empty() {
                push(vec![k.to_string(), v.to_string()]);
            }
        } else if VALUE_FLAGS.contains(&a)
            && let Some(v) = argv.get(i).filter(|v| !v.is_empty() && !v.starts_with('-'))
        {
            push(vec![a.to_string(), v.clone()]);
            i += 1;
        }
    }
    groups.into_iter().flatten().collect()
}

/// `flags` minus what `launcher` already passes, so a launcher that bakes in
/// `--dangerously-skip-permissions` (or a model) is not handed it twice.
fn flags_beyond(launcher: &str, flags: &[String]) -> Vec<String> {
    let have: Vec<&str> = launcher.split_whitespace().collect();
    let has_flag = |k: &str| {
        let eq = format!("{k}=");
        have.iter().any(|t| *t == k || t.starts_with(&eq))
    };
    let mut out = Vec::new();
    let mut i = 0;
    while i < flags.len() {
        let k = flags[i].as_str();
        if BOOL_FLAGS.contains(&k) {
            if !has_flag(k) {
                out.push(k.to_string());
            }
            i += 1;
            continue;
        }
        let v = flags.get(i + 1).cloned().unwrap_or_default();
        i += 2;
        let skip = if k == "--add-dir" {
            have.contains(&v.as_str())
        } else {
            has_flag(k)
        };
        if !skip && !v.is_empty() {
            out.push(k.to_string());
            out.push(v);
        }
    }
    out
}

/// The shell line typed into a restored pane:
/// `cd <cwd> && <launcher> <flags the launcher lacks> --resume=<sessionId>`.
pub fn resume_line(launcher: &str, cwd: &str, c: &ClaudeSnap) -> String {
    use crate::core::backend::{Prompt, launch_command};
    if c.harness != Harness::Claude {
        // Codex / pi keep only `--model <id>` (see `place`), under their own launcher.
        let model = c.flags.iter().skip_while(|f| *f != "--model").nth(1);
        let cmd = c.harness.resume_command(
            &c.harness.command(),
            model.map(String::as_str),
            &c.session_id,
        );
        return launch_command(cwd, Prompt::None, None, &cmd, c.harness);
    }
    let mut cmd = launcher.to_string();
    for f in flags_beyond(launcher, &c.flags) {
        cmd.push(' ');
        cmd.push_str(&tools::shq_min(&f));
    }
    // Bound to its flag (`--resume=<id>`): an id can never be read as a flag of its own.
    cmd.push_str(" --resume=");
    cmd.push_str(&tools::shq_min(&c.session_id));
    launch_command(cwd, Prompt::None, None, &cmd, Harness::Claude)
}

/// argv of each pid: `/proc/<pid>/cmdline` where there is one (exact), else one `ps` call
/// for all of them (split on whitespace — fine for the flags kept, which carry no spaces
/// unless an `--add-dir` path does).
fn argv_of(pids: &[i64]) -> HashMap<i64, Vec<String>> {
    let mut out = HashMap::new();
    let mut rest = Vec::new();
    for &pid in pids {
        match std::fs::read(format!("/proc/{pid}/cmdline")) {
            Ok(b) => {
                let argv = b
                    .split(|c| *c == 0)
                    .filter(|a| !a.is_empty())
                    .map(|a| String::from_utf8_lossy(a).to_string())
                    .collect();
                out.insert(pid, argv);
            }
            Err(_) => rest.push(pid.to_string()),
        }
    }
    if rest.is_empty() {
        return out;
    }
    let Ok(o) = std::process::Command::new("ps")
        .args(["-o", "pid=,command=", "-p", &rest.join(",")])
        .output()
    else {
        return out;
    };
    for line in String::from_utf8_lossy(&o.stdout).lines() {
        let mut it = line.split_whitespace();
        let Some(pid) = it.next().and_then(|p| p.parse::<i64>().ok()) else {
            continue;
        };
        out.insert(pid, it.map(str::to_string).collect());
    }
    out
}

// ------------------------------------------------------------------ live state

/// What is running right now, in snapshot shape.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Live {
    pub tmux: Vec<TmuxSnap>,
    pub iterm: Vec<ClaudeSnap>,
}

impl Live {
    /// Every session id in it.
    fn ids(&self) -> HashSet<String> {
        self.tmux
            .iter()
            .flat_map(TmuxSnap::claudes)
            .chain(self.iterm.iter())
            .map(|c| c.session_id.clone())
            .collect()
    }
}

const PANES_FMT: &str = "#{window_index}\t#{window_active}\t#{automatic-rename}\t#{pane_index}\t#{pane_active}\t#{pane_id}\t#{window_layout}\t#{pane_current_path}\t#{session_name}\t#{window_name}";

/// One `PANES_FMT` line → (session, window, pane, pane id). The window name comes last and
/// keeps any tab it contains.
fn parse_pane_line(line: &str) -> Option<(String, WindowSnap, PaneSnap, String)> {
    let mut p = line.splitn(10, '\t');
    let num = |s: Option<&str>| s.and_then(|v| v.trim().parse::<i64>().ok());
    let w_index = num(p.next())?;
    let w_active = p.next()?.trim() == "1";
    let auto = p.next()?.trim() != "0";
    let p_index = num(p.next())?;
    let p_active = p.next()?.trim() == "1";
    let pane_id = p.next()?.to_string();
    let layout = p.next()?.to_string();
    let cwd = p.next()?.to_string();
    let session = p.next()?.to_string();
    let w_name = p.next().unwrap_or("").to_string();
    if session.is_empty() {
        return None;
    }
    Some((
        session,
        WindowSnap {
            index: w_index,
            name: w_name,
            layout,
            active: w_active,
            auto_name: auto,
            ..Default::default()
        },
        PaneSnap {
            index: p_index,
            cwd,
            active: p_active,
            ..Default::default()
        },
        pane_id,
    ))
}

/// pane id → (session, window, pane) indices into the sessions it was built with.
type Positions = HashMap<String, (usize, usize, usize)>;

/// `list-panes -a` output → sessions (in tmux's order) → windows → panes, sorted by index.
fn build_layout(text: &str) -> (Vec<TmuxSnap>, Positions) {
    let mut sessions: Vec<TmuxSnap> = Vec::new();
    let mut ids: Vec<(String, String, i64, i64)> = Vec::new();
    for line in text.lines() {
        let Some((sname, win, pane, pane_id)) = parse_pane_line(line) else {
            continue;
        };
        ids.push((pane_id, sname.clone(), win.index, pane.index));
        let si = match sessions.iter().position(|s| s.name == sname) {
            Some(i) => i,
            None => {
                sessions.push(TmuxSnap {
                    name: sname,
                    ..Default::default()
                });
                sessions.len() - 1
            }
        };
        let s = &mut sessions[si];
        let wi = match s.windows.iter().position(|w| w.index == win.index) {
            Some(i) => i,
            None => {
                s.windows.push(win);
                s.windows.len() - 1
            }
        };
        s.windows[wi].panes.push(pane);
    }
    for s in &mut sessions {
        s.windows.sort_by_key(|w| w.index);
        for w in &mut s.windows {
            w.panes.sort_by_key(|p| p.index);
        }
    }
    let mut at = Positions::new();
    for (id, sname, wx, px) in ids {
        let Some(si) = sessions.iter().position(|s| s.name == sname) else {
            continue;
        };
        let Some(wi) = sessions[si].windows.iter().position(|w| w.index == wx) else {
            continue;
        };
        let Some(pi) = sessions[si].windows[wi]
            .panes
            .iter()
            .position(|p| p.index == px)
        else {
            continue;
        };
        at.insert(id, (si, wi, pi));
    }
    (sessions, at)
}

/// "No tmux server" in all the ways tmux says it.
fn is_no_server(why: &str) -> bool {
    why.contains("no server running")
        || why.contains("error connecting")
        || why.contains("No such file or directory")
}

/// Every tmux pane on the server. No server (or no tmux) is an empty list; a tmux that
/// errors otherwise is an error — an empty list there would read as "everything closed".
fn tmux_layout() -> std::result::Result<(Vec<TmuxSnap>, Positions), String> {
    let out = match tools::tmux_cmd()
        .args(["list-panes", "-a", "-F", PANES_FMT])
        .output()
    {
        Ok(o) => o,
        Err(_) => return Ok(Default::default()),
    };
    if !out.status.success() {
        let why = String::from_utf8_lossy(&out.stderr).trim().to_string();
        if is_no_server(&why) {
            return Ok(Default::default());
        }
        return Err(format!("tmux list-panes: {why}"));
    }
    Ok(build_layout(&String::from_utf8_lossy(&out.stdout)))
}

/// A discovery row as a [`ClaudeSnap`] (flags filled in separately).
fn claude_of(s: &Session) -> Option<ClaudeSnap> {
    Some(ClaudeSnap {
        session_id: s.session_id.clone().filter(|i| !i.is_empty())?,
        name: s.name.clone(),
        cwd: s.cwd.clone(),
        title: Some(s.display_title.clone().unwrap_or_else(|| s.headline())),
        harness: s.harness,
        ..Default::default()
    })
}

/// Place discovered rows into a tmux layout: each Claude session in its pane (by pane id),
/// the rest — iTerm, unknown, a pane on another server — on their own. Pure.
fn place(
    rows: &[Session],
    tmux: Vec<TmuxSnap>,
    at: &Positions,
    argv: &HashMap<i64, Vec<String>>,
) -> Live {
    let mut live = Live {
        tmux,
        iterm: Vec::new(),
    };
    for r in rows {
        let Some(mut c) = claude_of(r) else { continue };
        c.flags = if r.harness == Harness::Claude {
            argv.get(&r.pid)
                .map(|a| replayable_flags(a.get(1..).unwrap_or_default()))
                .unwrap_or_default()
        } else {
            // Codex / pi: the model is the one flag carried over (their own flags differ).
            r.model
                .iter()
                .flat_map(|m| ["--model".to_string(), m.clone()])
                .collect()
        };
        let pos = (r.backend == Backend::Tmux)
            .then(|| r.handle.as_deref().and_then(|h| at.get(h)))
            .flatten();
        match pos {
            Some(&(si, wi, pi)) => live.tmux[si].windows[wi].panes[pi].claude = Some(c),
            None => live.iterm.push(c),
        }
    }
    live
}

/// The live state: one `tmux list-panes -a`, one `ps` for the Claude pids.
pub fn gather(rows: &[Session]) -> std::result::Result<Live, String> {
    let (tmux, at) = tmux_layout()?;
    let argv = argv_of(&rows.iter().map(|r| r.pid).collect::<Vec<_>>());
    Ok(place(rows, tmux, &at, &argv))
}

// ------------------------------------------------------------------ record

/// `restore.keepClosedDays` in seconds; 0 = the recently-closed list is off.
pub fn keep_closed_secs() -> i64 {
    (crate::core::config::get().keep_closed_days() * 86_400.0).round() as i64
}

/// [`step`] with the default `restore.keepClosedDays`.
pub fn transition(
    stored: Snapshot,
    live: Live,
    boot: Option<&str>,
    host: &str,
    now: DateTime<Utc>,
) -> Snapshot {
    step(
        stored,
        live,
        boot,
        host,
        now,
        (DEFAULT_KEEP_CLOSED_DAYS * 86_400.0) as i64,
    )
}

/// The pure heart of [`record`]: `stored` moved forward to `live` at `now` on boot `boot`,
/// keeping closed entries for `keep` seconds (0 = none).
///
/// - A different boot id: everything the old boot was running moves into `dormant` (merged
///   by tmux name / session id, `since` = the old snapshot's `updatedAt`) — including what
///   was still lingering (`goneAt`). Older closed entries stay closed.
/// - Then `tmux` / `iterm` become the live state. What vanished since the last record
///   lingers with `goneAt` until [`GRACE_SECS`] have passed (see the module docs) — shown
///   as closed meanwhile ([`closed_entries`]) — and then moves into `closed`.
/// - A dormant or closed Claude session that is live again (resumed by hand) leaves
///   `dormant` / `closed`; a dormant tmux session left with no dormant Claude pane, whose
///   name is live again, too. A session id is never both dormant and closed.
pub fn step(
    mut stored: Snapshot,
    live: Live,
    boot: Option<&str>,
    host: &str,
    now: DateTime<Utc>,
    keep: i64,
) -> Snapshot {
    let rebooted = matches!((stored.boot_id.as_deref(), boot), (Some(a), Some(b)) if a != b);
    if rebooted {
        let since = stored.updated_at.clone().unwrap_or_else(|| iso(now));
        for mut t in std::mem::take(&mut stored.tmux) {
            t.since = Some(since.clone());
            t.gone_at = None;
            for p in t.windows.iter_mut().flat_map(|w| w.panes.iter_mut()) {
                if let Some(c) = p.claude.as_mut() {
                    c.gone_at = None;
                }
            }
            stored.dormant.tmux.retain(|d| d.name != t.name);
            stored.dormant.tmux.push(t);
        }
        for mut c in std::mem::take(&mut stored.iterm) {
            c.since = Some(since.clone());
            c.gone_at = None;
            stored
                .dormant
                .iterm
                .retain(|d| d.session_id != c.session_id);
            stored.dormant.iterm.push(c);
        }
        // A session recorded in a dormant tmux pane is restored with it, not on its own.
        let in_tmux: HashSet<String> = stored
            .dormant
            .tmux
            .iter()
            .flat_map(TmuxSnap::claudes)
            .map(|c| c.session_id.clone())
            .collect();
        stored
            .dormant
            .iterm
            .retain(|c| !in_tmux.contains(&c.session_id));
    }
    if boot.is_some() {
        stored.boot_id = boot.map(str::to_string);
    }

    let live_ids = live.ids();
    let live_names: HashSet<String> = live.tmux.iter().map(|t| t.name.clone()).collect();
    let (tmux, iterm, closed) = linger(
        std::mem::take(&mut stored.tmux),
        std::mem::take(&mut stored.iterm),
        live,
        &live_ids,
        now,
    );
    stored.tmux = tmux;
    stored.iterm = iterm;

    stored
        .dormant
        .iterm
        .retain(|c| !live_ids.contains(&c.session_id));
    for t in stored.dormant.tmux.iter_mut() {
        for p in t.windows.iter_mut().flat_map(|w| w.panes.iter_mut()) {
            // Resumed by hand somewhere: a restore must not start it a second time.
            if p.claude
                .as_ref()
                .is_some_and(|c| live_ids.contains(&c.session_id))
            {
                p.claude = None;
            }
        }
    }
    stored
        .dormant
        .tmux
        .retain(|t| !(live_names.contains(&t.name) && t.claudes().next().is_none()));

    if keep > 0 {
        for c in closed {
            match c {
                Taken::Tmux(t) => stored.closed.add_tmux(t),
                Taken::Claude(c) => stored.closed.add_claude(c),
            }
        }
        let mut not_closed = live_ids;
        not_closed.extend(dormant_ids(&stored));
        stored.closed.drop_ids(&not_closed);
        stored.closed.expire(keep, now);
    } else {
        stored.closed.tmux.clear();
        stored.closed.claude.clear();
    }

    stored.version = VERSION;
    stored.host = host.to_string();
    stored
}

/// The live lists, plus what vanished from the stored ones less than [`GRACE_SECS`] ago
/// (stamped `goneAt` the first time it is missed), plus — third — what has been gone longer
/// and is now closed (`closedAt` = its `goneAt`). A vanished tmux session lingers whole; a
/// Claude session gone from a pane that is still there goes back into that pane, else into
/// the non-tmux list (remembering its tmux session).
fn linger(
    stored_tmux: Vec<TmuxSnap>,
    stored_iterm: Vec<ClaudeSnap>,
    live: Live,
    live_ids: &HashSet<String>,
    now: DateTime<Utc>,
) -> (Vec<TmuxSnap>, Vec<ClaudeSnap>, Vec<Taken>) {
    // Ok(goneAt) while it lingers, Err(closedAt) once the grace period is over.
    let fresh = |gone: &Option<String>| -> std::result::Result<String, String> {
        match gone {
            None => Ok(iso(now)),
            Some(g) => match age(g, now) {
                Some(a) if a <= GRACE_SECS => Ok(g.clone()),
                Some(_) => Err(g.clone()),
                None => Err(iso(now)),
            },
        }
    };
    let Live {
        mut tmux,
        mut iterm,
    } = live;
    let mut closed: Vec<Taken> = Vec::new();
    let mut orphans: Vec<ClaudeSnap> = Vec::new();
    for mut st in stored_tmux {
        if let Some(lt) = tmux.iter_mut().find(|t| t.name == st.name) {
            for w in &st.windows {
                for p in &w.panes {
                    let Some(c) = p.claude.as_ref() else { continue };
                    if live_ids.contains(&c.session_id) {
                        continue;
                    }
                    let c = ClaudeSnap {
                        tmux_session: Some(st.name.clone()),
                        ..c.clone()
                    };
                    let gone = match fresh(&c.gone_at) {
                        Ok(g) => g,
                        Err(at) => {
                            closed.push(Taken::Claude(ClaudeSnap {
                                closed_at: Some(at),
                                ..c
                            }));
                            continue;
                        }
                    };
                    let c = ClaudeSnap {
                        gone_at: Some(gone),
                        ..c
                    };
                    let slot = lt
                        .windows
                        .iter_mut()
                        .find(|lw| lw.index == w.index)
                        .and_then(|lw| lw.panes.iter_mut().find(|lp| lp.index == p.index))
                        .filter(|lp| lp.claude.is_none());
                    match slot {
                        Some(lp) => lp.claude = Some(c),
                        None => orphans.push(c),
                    }
                }
            }
            continue;
        }
        // Its Claude sessions alive elsewhere are not part of it any more.
        for p in st.windows.iter_mut().flat_map(|w| w.panes.iter_mut()) {
            if p.claude
                .as_ref()
                .is_some_and(|c| live_ids.contains(&c.session_id))
            {
                p.claude = None;
            }
        }
        match fresh(&st.gone_at) {
            Ok(gone) => {
                st.gone_at = Some(gone);
                tmux.push(st);
            }
            Err(at) => {
                st.gone_at = None;
                st.closed_at = Some(at);
                closed.push(Taken::Tmux(st));
            }
        }
    }
    for c in stored_iterm.into_iter().chain(orphans) {
        if live_ids.contains(&c.session_id) || iterm.iter().any(|i| i.session_id == c.session_id) {
            continue;
        }
        match fresh(&c.gone_at) {
            Ok(gone) => iterm.push(ClaudeSnap {
                gone_at: Some(gone),
                ..c
            }),
            Err(at) => closed.push(Taken::Claude(ClaudeSnap {
                closed_at: Some(at),
                ..c
            })),
        }
    }
    (tmux, iterm, closed)
}

/// Record the live state on top of the stored snapshot. Writes only when something changed
/// (or [`REFRESH_SECS`] passed), never in fixture mode, and never on a tmux that errored.
/// `rows` must come from a discovery that *succeeded* — an unreadable registry is not an
/// empty fleet. → whether it wrote.
pub fn record(rows: &[Session]) -> Result<bool> {
    if discovery::is_fixture() {
        return Ok(false);
    }
    let live = gather(rows).map_err(Error::Other)?;
    let boot = boot_id();
    let host = host_name();
    let p = path();
    let _guard = lock(&p)?;
    let stored = match load_from(&p) {
        Ok(s) => s,
        Err(e) => {
            // Never wedge recording forever on one bad file: keep it aside, start over.
            let mut aside = p.as_os_str().to_owned();
            aside.push(".corrupt");
            let _ = std::fs::rename(&p, PathBuf::from(aside));
            crate::core::hosts::debug(&format!("snapshot: {e} — moved aside"));
            Snapshot::default()
        }
    };
    let now = Utc::now();
    let next = step(
        stored.clone(),
        live,
        boot.as_deref(),
        &host,
        now,
        keep_closed_secs(),
    );
    let stale = stored
        .updated_at
        .as_deref()
        .and_then(|u| age(u, now))
        .is_none_or(|a| a >= REFRESH_SECS);
    if next.same_content(&stored) && !stale {
        return Ok(false);
    }
    let mut next = next;
    next.updated_at = Some(iso(now));
    save_to(&p, &next)?;
    Ok(true)
}

/// [`record`] that never fails its caller: a problem is logged under `FLEET_DEBUG`.
pub fn record_quietly(rows: &[Session]) {
    if let Err(e) = record(rows) {
        crate::core::hosts::debug(&format!("snapshot not recorded: {e}"));
    }
}

/// Record from a fresh discovery, then return the snapshot — what the restore paths start
/// from, so the first command after a reboot already sees the old boot as dormant.
pub fn refresh() -> Result<Snapshot> {
    if let Ok(mut rows) = discovery::discover_checked() {
        crate::core::naming::stamp_titles(&mut rows);
        crate::core::title::stamp_display_titles(&mut rows);
        record_quietly(&rows);
    }
    current()
}

// ------------------------------------------------------------------ reading

/// The stored snapshot as of *this* boot: when it was written in an earlier one, its live
/// lists are shown as dormant (without writing — [`record`] does that). Missing file =
/// empty snapshot.
pub fn current() -> Result<Snapshot> {
    let s = load_from(&path())?;
    let boot = boot_id();
    let rebooted = matches!((s.boot_id.as_deref(), boot.as_deref()), (Some(a), Some(b)) if a != b);
    if !rebooted {
        return Ok(s);
    }
    let host = s.host.clone();
    Ok(step(
        s,
        Live::default(),
        boot.as_deref(),
        &host,
        Utc::now(),
        keep_closed_secs(),
    ))
}

/// Session ids of every dormant Claude session (in a dormant tmux pane or on its own) —
/// what grouping and stacks treat as "still there". Empty on any error.
pub fn dormant_ids(s: &Snapshot) -> HashSet<String> {
    s.dormant
        .tmux
        .iter()
        .flat_map(TmuxSnap::claudes)
        .chain(s.dormant.iterm.iter())
        .map(|c| c.session_id.clone())
        .collect()
}

/// [`dormant_ids`] of this machine's [`current`] snapshot; empty when it can't be read.
pub fn dormant_session_ids() -> HashSet<String> {
    dormant_session_ids_checked().unwrap_or_default()
}

/// [`dormant_session_ids`], but `None` when the snapshot can't be read — so a caller that
/// prunes on absence (grouping) can hold off instead. Empty in fixture mode unless a test
/// names `$FLEET_SNAPSHOT` (a demo must not read the real state file).
pub fn dormant_session_ids_checked() -> Option<HashSet<String>> {
    if discovery::is_fixture() && std::env::var_os("FLEET_SNAPSHOT").is_none() {
        return Some(HashSet::new());
    }
    current().ok().map(|s| dormant_ids(&s))
}

/// How many dormant entries this machine has (for the `list` footer and the dashboard).
/// Always 0 in fixture mode, whose canned fleet has no history.
pub fn dormant_count() -> usize {
    if discovery::is_fixture() {
        return 0;
    }
    current().map(|s| s.dormant.len()).unwrap_or(0)
}

/// One Claude session inside a [`DormantView`].
#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct DormantSession {
    pub session_id: String,
    pub name: Option<String>,
    pub title: Option<String>,
    pub cwd: Option<String>,
}

/// One dormant entry as the CLI and the web API show it (`fleet restore --json`).
#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct DormantView {
    /// `"tmux"` (a tmux session) or `"claude"` (a Claude session that was not in tmux).
    pub kind: &'static str,
    /// What `fleet restore <target>` takes: the tmux name, or the full session id.
    pub target: String,
    /// The tmux session name, or the Claude session's title.
    pub name: String,
    pub since: Option<String>,
    pub windows: usize,
    pub panes: usize,
    pub sessions: Vec<DormantSession>,
}

fn session_view(c: &ClaudeSnap) -> DormantSession {
    DormantSession {
        session_id: c.session_id.clone(),
        name: c.name.clone(),
        title: c.title.clone(),
        cwd: c.cwd.clone(),
    }
}

/// Every dormant entry: tmux sessions first, then lone Claude sessions, each most recent first.
pub fn dormant_views(s: &Snapshot) -> Vec<DormantView> {
    let mut tmux: Vec<DormantView> = s
        .dormant
        .tmux
        .iter()
        .map(|t| DormantView {
            kind: "tmux",
            target: t.name.clone(),
            name: t.name.clone(),
            since: t.since.clone(),
            windows: t.windows.len(),
            panes: t.pane_count(),
            sessions: t.claudes().map(session_view).collect(),
        })
        .collect();
    let mut claude: Vec<DormantView> = s
        .dormant
        .iterm
        .iter()
        .map(|c| DormantView {
            kind: "claude",
            target: c.session_id.clone(),
            name: c.label(),
            since: c.since.clone(),
            windows: 1,
            panes: 1,
            sessions: vec![session_view(c)],
        })
        .collect();
    let newest = |a: &DormantView, b: &DormantView| b.since.cmp(&a.since);
    tmux.sort_by(newest);
    claude.sort_by(newest);
    tmux.extend(claude);
    tmux
}

/// One recently closed entry (`fleet restore --closed --json`, and `closed` in the list):
/// a [`DormantView`] (`since` = `closedAt`) plus when it closed and, for a Claude session
/// whose tmux pane outlived it, that tmux session.
#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ClosedView {
    #[serde(flatten)]
    pub view: DormantView,
    pub closed_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tmux_session: Option<String>,
}

/// Everything recently closed: the stored `closed` lists, plus what is still lingering
/// (`goneAt`) — it shows as closed at once, while a reboot within [`GRACE_SECS`] would still
/// make it dormant instead. Lone Claude sessions in `iterm`; a session id appears once.
pub fn closed_entries(s: &Snapshot) -> Closed {
    let mut out = s.closed.clone();
    let mut lingering: Vec<(String, Taken)> = Vec::new();
    for t in &s.tmux {
        if let Some(g) = &t.gone_at {
            lingering.push((
                g.clone(),
                Taken::Tmux(TmuxSnap {
                    gone_at: None,
                    closed_at: Some(g.clone()),
                    ..t.clone()
                }),
            ));
            continue;
        }
        for c in t.claudes() {
            if let Some(g) = &c.gone_at {
                lingering.push((
                    g.clone(),
                    Taken::Claude(ClaudeSnap {
                        closed_at: Some(g.clone()),
                        tmux_session: Some(t.name.clone()),
                        ..c.clone()
                    }),
                ));
            }
        }
    }
    for c in &s.iterm {
        if let Some(g) = &c.gone_at {
            lingering.push((
                g.clone(),
                Taken::Claude(ClaudeSnap {
                    closed_at: Some(g.clone()),
                    ..c.clone()
                }),
            ));
        }
    }
    // Oldest first: each add counts as newer than what is there.
    lingering.sort_by(|a, b| a.0.cmp(&b.0));
    for (_, x) in lingering {
        match x {
            Taken::Tmux(t) => out.add_tmux(t),
            Taken::Claude(c) => out.add_claude(c),
        }
    }
    out
}

/// [`closed_entries`] as a [`Dormant`] (lone sessions in `iterm`) — what targets resolve in.
fn closed_pool(s: &Snapshot) -> Dormant {
    let c = closed_entries(s);
    Dormant {
        tmux: c.tmux,
        iterm: c.claude,
        extra: Map::new(),
    }
}

/// Every recently closed entry, most recent first.
pub fn closed_views(s: &Snapshot) -> Vec<ClosedView> {
    let c = closed_entries(s);
    let mut out: Vec<ClosedView> = c
        .tmux
        .iter()
        .map(|t| ClosedView {
            view: DormantView {
                kind: "tmux",
                target: t.name.clone(),
                name: t.name.clone(),
                since: t.closed_at.clone(),
                windows: t.windows.len(),
                panes: t.pane_count(),
                sessions: t.claudes().map(session_view).collect(),
            },
            closed_at: t.closed_at.clone(),
            tmux_session: None,
        })
        .chain(c.claude.iter().map(|c| ClosedView {
            view: DormantView {
                kind: "claude",
                target: c.session_id.clone(),
                name: c.label(),
                since: c.closed_at.clone(),
                windows: 1,
                panes: 1,
                sessions: vec![session_view(c)],
            },
            closed_at: c.closed_at.clone(),
            tmux_session: c.tmux_session.clone(),
        }))
        .collect();
    out.sort_by(|a, b| b.closed_at.cmp(&a.closed_at));
    out
}

/// [`closed_views`] of this machine's [`current`] snapshot, empty when the list is off
/// (`restore.keepClosedDays: 0`).
pub fn closed_now(s: &Snapshot) -> Vec<ClosedView> {
    if keep_closed_secs() <= 0 {
        return Vec::new();
    }
    closed_views(s)
}

// ------------------------------------------------------------------ targets

/// Which list a target lives in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pool {
    /// Left by a reboot (`dormant`).
    Dormant,
    /// Ended within a boot (`closed` and what lingers).
    Closed,
}

impl Pool {
    fn noun(self) -> &'static str {
        match self {
            Pool::Dormant => "dormant",
            Pool::Closed => "recently closed",
        }
    }

    /// The entries of this pool in `s`.
    pub fn entries(self, s: &Snapshot) -> Dormant {
        match self {
            Pool::Dormant => s.dormant.clone(),
            Pool::Closed => closed_pool(s),
        }
    }
}

/// One dormant entry, by identity.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Target {
    /// A dormant tmux session, by name.
    Tmux(String),
    /// A dormant Claude session outside tmux, by full session id.
    Claude(String),
}

impl Target {
    pub fn label(&self, s: &Snapshot) -> String {
        self.label_in(&s.dormant)
    }

    /// Its name among `d`'s entries.
    pub fn label_in(&self, d: &Dormant) -> String {
        match self {
            Target::Tmux(n) => n.clone(),
            Target::Claude(id) => d
                .iterm
                .iter()
                .find(|c| &c.session_id == id)
                .map(ClaudeSnap::label)
                .unwrap_or_else(|| id.clone()),
        }
    }
}

/// Resolve `q` to exactly one dormant entry, or exit 2 (ambiguous, with the candidates) /
/// 3 (nothing). Tried in order, the first step with any hit deciding:
/// 1. a dormant tmux name — exact > case-folded > prefix > substring, like `enter`;
/// 2. a session id, whole or a prefix of at least 4 characters — a session in a dormant
///    tmux pane resolves to that tmux session;
/// 3. a recorded title or name — exact, then prefix, then substring (case-insensitive).
pub fn resolve(s: &Snapshot, q: &str) -> Result<Target> {
    resolve_in(&s.dormant, q, Pool::Dormant)
}

/// [`resolve`] among the recently closed entries ([`closed_entries`]).
pub fn resolve_closed(s: &Snapshot, q: &str) -> Result<Target> {
    resolve_in(&closed_pool(s), q, Pool::Closed)
}

fn resolve_in(d: &Dormant, q: &str, pool: Pool) -> Result<Target> {
    let q = q.trim();
    if q.is_empty() {
        return Err(Error::exit(1, "no target given"));
    }
    let names: Vec<crate::core::tmux::TmuxSession> = d
        .tmux
        .iter()
        .map(|t| crate::core::tmux::TmuxSession {
            name: t.name.clone(),
            ..Default::default()
        })
        .collect();
    if let Some((_, hits)) = crate::core::tmux::match_tier(&names, q) {
        return one(d, hits.into_iter().map(Target::Tmux).collect(), q, pool);
    }
    // Every dormant Claude session with the entry it restores through.
    let mut all: Vec<(&ClaudeSnap, Target)> = Vec::new();
    for t in &d.tmux {
        for c in t.claudes() {
            all.push((c, Target::Tmux(t.name.clone())));
        }
    }
    for c in &d.iterm {
        all.push((c, Target::Claude(c.session_id.clone())));
    }
    let ql = q.to_lowercase();
    if ql.len() >= MIN_ID_PREFIX {
        let hits: Vec<Target> = all
            .iter()
            .filter(|(c, _)| c.session_id.to_lowercase().starts_with(&ql))
            .map(|(_, t)| t.clone())
            .collect();
        if !hits.is_empty() {
            return one(d, hits, q, pool);
        }
    }
    let texts = |c: &ClaudeSnap| {
        [c.title.as_deref(), c.name.as_deref()]
            .into_iter()
            .flatten()
            .map(str::to_lowercase)
            .collect::<Vec<_>>()
    };
    let tiers: [&dyn Fn(&str) -> bool; 3] = [
        &|t: &str| t == ql,
        &|t: &str| t.starts_with(&ql),
        &|t: &str| t.contains(&ql),
    ];
    for tier in tiers {
        let hits: Vec<Target> = all
            .iter()
            .filter(|(c, _)| texts(c).iter().any(|t| tier(t)))
            .map(|(_, t)| t.clone())
            .collect();
        if !hits.is_empty() {
            return one(d, hits, q, pool);
        }
    }
    Err(Error::exit(
        EXIT_NOTHING,
        format!("no {} session matches \"{q}\"", pool.noun()),
    ))
}

/// One target from a tier's hits (several hits naming the same entry are one), else exit 2.
fn one(d: &Dormant, mut hits: Vec<Target>, q: &str, pool: Pool) -> Result<Target> {
    let mut seen = HashSet::new();
    hits.retain(|t| seen.insert(t.clone()));
    if hits.len() == 1 {
        return Ok(hits.remove(0));
    }
    let names: Vec<String> = hits.iter().map(|t| t.label_in(d)).collect();
    Err(Error::exit(
        EXIT_AMBIGUOUS,
        format!(
            "\"{q}\" matches {} {} sessions: {} — be more specific",
            names.len(),
            pool.noun(),
            names.join(", ")
        ),
    ))
}

// ------------------------------------------------------------------ restore

/// One Claude session a restore launched (or, dry, would launch).
#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Launched {
    pub session_id: String,
    pub title: Option<String>,
    pub line: String,
}

/// What [`restore`] did. `fleet restore <target> --json` prints these.
#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Restored {
    pub kind: &'static str,
    /// The dormant entry's name (tmux name, or the Claude session's title).
    pub from: String,
    /// The tmux session it now lives in.
    pub session: String,
    /// `session` differs from the recorded name (that name was taken, or it was not tmux).
    pub renamed: bool,
    pub windows: usize,
    pub panes: usize,
    pub launched: Vec<Launched>,
    pub warnings: Vec<String>,
    /// Every tmux command and launch line, in order (what a dry run would print).
    pub commands: Vec<String>,
    pub dry_run: bool,
}

/// Runs (or, dry, only renders) the tmux commands of one restore.
struct Exec {
    dry: bool,
    log: Vec<String>,
}

impl Exec {
    fn render(args: &[String]) -> String {
        let mut shown = tools::shq_min(tools::tmux());
        for a in args {
            shown.push(' ');
            shown.push_str(&tools::shq_min(a));
        }
        shown
    }

    /// Run one tmux command; its stdout (trimmed). Dry: `placeholder`.
    fn run(&mut self, args: Vec<String>, placeholder: &str) -> Result<String> {
        self.log.push(Self::render(&args));
        if self.dry {
            return Ok(placeholder.to_string());
        }
        let out = tools::tmux_cmd().args(&args).output()?;
        if !out.status.success() {
            let why = String::from_utf8_lossy(&out.stderr).trim().to_string();
            return Err(Error::Other(format!(
                "tmux {}: {}",
                args.first().map(String::as_str).unwrap_or(""),
                if why.is_empty() {
                    format!("exited {}", out.status)
                } else {
                    why
                }
            )));
        }
        Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
    }

    /// [`Exec::run`] whose failure is a warning, not the end of the restore.
    fn soft(&mut self, args: Vec<String>, warnings: &mut Vec<String>) {
        if let Err(e) = self.run(args, "") {
            warnings.push(e.to_string());
        }
    }

    /// Type `line` into `pane` and submit it (spawn's two-step `send-keys`).
    fn type_line(&mut self, pane: &str, line: &str) -> Result<()> {
        self.run(
            vec![
                "send-keys".into(),
                "-t".into(),
                pane.into(),
                "-l".into(),
                line.into(),
            ],
            "",
        )?;
        self.run(
            vec!["send-keys".into(), "-t".into(), pane.into(), "Enter".into()],
            "",
        )?;
        Ok(())
    }
}

fn s(v: &str) -> String {
    v.to_string()
}

/// `cwd`, or `$HOME` with a warning when it no longer exists.
fn usable_dir(cwd: &str, warnings: &mut Vec<String>) -> String {
    let expanded = tools::expand_tilde(cwd);
    if !expanded.is_empty() && Path::new(&expanded).is_dir() {
        return expanded;
    }
    let home = dirs::home_dir()
        .map(|h| h.display().to_string())
        .unwrap_or_else(|| "/".into());
    warnings.push(format!(
        "{} no longer exists — using {}",
        tools::tildify(cwd),
        tools::tildify(&home)
    ));
    home
}

/// `WxH` from a `#{window_layout}` (`csum,WxH,X,Y…`).
fn layout_size(layout: &str) -> Option<(u32, u32)> {
    let dims = layout.split(',').nth(1)?;
    let (w, h) = dims.split_once('x')?;
    let h: String = h.chars().take_while(char::is_ascii_digit).collect();
    Some((w.parse().ok()?, h.parse().ok()?))
}

/// The tmux name a restore lands under: the recorded one, or `<name>-restored` (then
/// `-restored-2`, …) when a live session already has it.
pub fn restore_name(name: &str, live: &[String]) -> String {
    if !live.iter().any(|l| l == name) {
        return name.to_string();
    }
    crate::core::backend::unique_tmux_name(&format!("{name}-restored"), live)
}

/// The commands that rebuild `t` as tmux session `name`, run through `ex`. Claude panes
/// whose session is in `skip` (live again elsewhere) get a shell only. Pure apart from `ex`.
fn rebuild(
    ex: &mut Exec,
    t: &TmuxSnap,
    name: &str,
    launcher: &str,
    skip: &HashSet<String>,
    warnings: &mut Vec<String>,
) -> Result<Vec<Launched>> {
    let mut pending: Vec<(String, String, &ClaudeSnap)> = Vec::new();
    let mut active_window: Option<String> = None;
    let windows: Vec<&WindowSnap> = t.windows.iter().filter(|w| !w.panes.is_empty()).collect();
    if windows.is_empty() {
        return Err(Error::Other(format!(
            "dormant session {} has no panes recorded",
            t.name
        )));
    }
    for (wi, w) in windows.iter().enumerate() {
        let dirs: Vec<String> = w
            .panes
            .iter()
            .map(|p| usable_dir(&p.cwd, warnings))
            .collect();
        let mut args = if wi == 0 {
            // Splits and later windows inherit the session's environment.
            let mut a = vec![
                s("new-session"),
                s("-d"),
                s("-s"),
                s(name),
                s("-e"),
                s(crate::core::backend::SPAWN_SHELL_ENV),
            ];
            // Big enough for every split; an attaching client resizes it anyway.
            if let Some((x, y)) = layout_size(&w.layout) {
                a.extend([s("-x"), x.to_string(), s("-y"), y.to_string()]);
            }
            a
        } else {
            vec![s("new-window"), s("-d"), s("-t"), format!("={name}:")]
        };
        if !w.auto_name && !w.name.is_empty() {
            args.extend([s("-n"), w.name.clone()]);
        }
        args.extend([s("-c"), dirs[0].clone(), s("-P"), s("-F"), s("#{pane_id}")]);
        let first = ex.run(args, &format!("%{}.{}", w.index, w.panes[0].index))?;
        let mut ids = vec![first.clone()];
        for (pi, p) in w.panes.iter().enumerate().skip(1) {
            let id = ex.run(
                vec![
                    s("split-window"),
                    s("-d"),
                    s("-t"),
                    ids[pi - 1].clone(),
                    s("-c"),
                    dirs[pi].clone(),
                    s("-P"),
                    s("-F"),
                    s("#{pane_id}"),
                ],
                &format!("%{}.{}", w.index, p.index),
            )?;
            ids.push(id);
            // Keep every pane splittable: repeated halving runs out of room fast.
            if pi + 1 < w.panes.len() {
                ex.soft(
                    vec![s("select-layout"), s("-t"), first.clone(), s("tiled")],
                    warnings,
                );
            }
        }
        if w.panes.len() > 1 && !w.layout.is_empty() {
            ex.soft(
                vec![s("select-layout"), s("-t"), first.clone(), w.layout.clone()],
                warnings,
            );
        }
        if let Some(pi) = w.panes.iter().position(|p| p.active)
            && w.panes.len() > 1
        {
            ex.soft(vec![s("select-pane"), s("-t"), ids[pi].clone()], warnings);
        }
        if w.active {
            active_window = Some(first.clone());
        }
        for (pi, p) in w.panes.iter().enumerate() {
            if let Some(c) = p.claude.as_ref() {
                if skip.contains(&c.session_id) {
                    warnings.push(format!(
                        "{} is already running elsewhere — not started again",
                        c.label()
                    ));
                    continue;
                }
                let dir = match c.cwd.as_deref() {
                    Some(d) if d != p.cwd => usable_dir(d, warnings),
                    _ => dirs[pi].clone(),
                };
                pending.push((ids[pi].clone(), dir, c));
            }
        }
    }
    if windows.len() > 1
        && let Some(w) = active_window
    {
        ex.soft(vec![s("select-window"), s("-t"), w], warnings);
    }
    let mut launched = Vec::new();
    for (pane, dir, c) in pending {
        let line = resume_line(launcher, &dir, c);
        ex.type_line(&pane, &line)?;
        launched.push(Launched {
            session_id: c.session_id.clone(),
            title: c.title.clone(),
            line,
        });
    }
    Ok(launched)
}

/// A lone dormant Claude session as a one-pane tmux layout, named after its title.
fn as_tmux(c: &ClaudeSnap, live: &[String]) -> TmuxSnap {
    let base = crate::core::tmux::sanitize_name(&c.label());
    let base: String = base.chars().take(40).collect();
    let base = base.trim_matches('-').to_string();
    let base = if base.is_empty() { s("claude") } else { base };
    TmuxSnap {
        name: crate::core::backend::unique_tmux_name(&base, live),
        windows: vec![WindowSnap {
            index: 0,
            active: true,
            auto_name: true,
            panes: vec![PaneSnap {
                index: 0,
                cwd: c.cwd.clone().unwrap_or_else(|| "~".into()),
                active: true,
                claude: Some(c.clone()),
                ..Default::default()
            }],
            ..Default::default()
        }],
        ..Default::default()
    }
}

/// Take `target` out of `pool` (under the lock) and hand it back.
fn take(pool: Pool, target: &Target) -> Result<Result<Taken>> {
    update(|snap| {
        let found = match (pool, target) {
            (Pool::Closed, t) => {
                let entry = match t {
                    Target::Tmux(n) => closed_entries(snap)
                        .tmux
                        .into_iter()
                        .find(|x| &x.name == n)
                        .map(Taken::Tmux),
                    Target::Claude(id) => closed_entries(snap)
                        .claude
                        .into_iter()
                        .find(|x| &x.session_id == id)
                        .map(Taken::Claude),
                };
                entry.filter(|_| remove_closed(snap, t))
            }
            (Pool::Dormant, Target::Tmux(n)) => snap
                .dormant
                .tmux
                .iter()
                .position(|t| &t.name == n)
                .map(|i| Taken::Tmux(snap.dormant.tmux.remove(i))),
            (Pool::Dormant, Target::Claude(id)) => snap
                .dormant
                .iterm
                .iter()
                .position(|c| &c.session_id == id)
                .map(|i| Taken::Claude(snap.dormant.iterm.remove(i))),
        };
        Ok(match found {
            Some(t) => (true, Ok(t)),
            None => (
                false,
                Err(Error::exit(
                    EXIT_NOTHING,
                    format!(
                        "that session is no longer {} (restored or forgotten meanwhile)",
                        pool.noun()
                    ),
                )),
            ),
        })
    })
}

/// Take one closed entry out of the snapshot, wherever it is kept: the `closed` lists, or
/// still lingering (`goneAt`) in `tmux` / `iterm` / a live pane. → whether it was there.
fn remove_closed(snap: &mut Snapshot, target: &Target) -> bool {
    match target {
        Target::Tmux(n) => {
            if let Some(i) = snap
                .tmux
                .iter()
                .position(|t| &t.name == n && t.gone_at.is_some())
            {
                snap.tmux.remove(i);
                // An older closed session of that name showed as lone sessions: keep them so.
                if let Some(j) = snap.closed.tmux.iter().position(|t| &t.name == n) {
                    let old = snap.closed.tmux.remove(j);
                    for c in old.claudes() {
                        snap.closed.claude.push(ClaudeSnap {
                            closed_at: old.closed_at.clone(),
                            tmux_session: Some(old.name.clone()),
                            ..c.clone()
                        });
                    }
                }
                return true;
            }
            let before = snap.closed.tmux.len();
            snap.closed.tmux.retain(|t| &t.name != n);
            snap.closed.tmux.len() != before
        }
        Target::Claude(id) => {
            let mut hit = snap.closed.ids().contains(id);
            snap.closed.drop_ids(&HashSet::from([id.clone()]));
            let before = snap.iterm.len();
            snap.iterm
                .retain(|c| !(&c.session_id == id && c.gone_at.is_some()));
            hit |= snap.iterm.len() != before;
            for t in snap.tmux.iter_mut().filter(|t| t.gone_at.is_none()) {
                for p in t.windows.iter_mut().flat_map(|w| w.panes.iter_mut()) {
                    if p.claude
                        .as_ref()
                        .is_some_and(|c| &c.session_id == id && c.gone_at.is_some())
                    {
                        p.claude = None;
                        hit = true;
                    }
                }
            }
            hit
        }
    }
}

#[derive(Debug, Clone)]
enum Taken {
    Tmux(TmuxSnap),
    Claude(ClaudeSnap),
}

/// Put a taken entry back after a failed restore (unless something re-added it). A closed
/// one goes into the `closed` lists, keeping its `closedAt`.
fn put_back(pool: Pool, taken: Taken) {
    let _ = update(|snap| {
        match (pool, taken) {
            (Pool::Dormant, Taken::Tmux(t)) => {
                if snap.dormant.tmux.iter().any(|d| d.name == t.name) {
                    return Ok((false, ()));
                }
                snap.dormant.tmux.push(t);
            }
            (Pool::Dormant, Taken::Claude(c)) => {
                if snap
                    .dormant
                    .iterm
                    .iter()
                    .any(|d| d.session_id == c.session_id)
                {
                    return Ok((false, ()));
                }
                snap.dormant.iterm.push(c);
            }
            (Pool::Closed, Taken::Tmux(t)) => {
                let all = closed_entries(snap);
                if all.tmux.iter().any(|d| d.name == t.name) {
                    return Ok((false, ()));
                }
                let ids = all.ids();
                if t.claudes().any(|c| ids.contains(&c.session_id)) {
                    return Ok((false, ()));
                }
                snap.closed.tmux.push(t);
            }
            (Pool::Closed, Taken::Claude(c)) => {
                if closed_entries(snap).ids().contains(&c.session_id) {
                    return Ok((false, ()));
                }
                snap.closed.claude.push(c);
            }
        }
        Ok((true, ()))
    });
}

/// Bring one dormant entry back (see the module docs), with `launcher` as the command that
/// starts Claude (what spawn uses). A tmux session is rebuilt under its own name, or
/// `<name>-restored` when that is taken; a lone Claude session lands in a new tmux session
/// named after its title — tmux is the backend a restore can drive reliably. On success the
/// entry leaves the dormant list; a dry run (`-n`) changes nothing and only renders.
pub fn restore(snap: &Snapshot, target: &Target, launcher: &str) -> Result<Restored> {
    restore_from(snap, Pool::Dormant, target, launcher)
}

/// [`restore`] for a recently closed entry. The same, except that a Claude session whose
/// tmux session outlived it (`/exit`, the shell stayed) comes back in a new window of that
/// session while it exists.
pub fn restore_closed(snap: &Snapshot, target: &Target, launcher: &str) -> Result<Restored> {
    restore_from(snap, Pool::Closed, target, launcher)
}

fn restore_from(snap: &Snapshot, pool: Pool, target: &Target, launcher: &str) -> Result<Restored> {
    let dry = crate::core::hosts::dry_run();
    if discovery::is_fixture() && !dry {
        return Err(Error::Other(
            "fixture mode: restore is inert (unset FLEET_FIXTURE)".into(),
        ));
    }
    let entries = pool.entries(snap);
    let entry = match target {
        Target::Tmux(n) => entries
            .tmux
            .iter()
            .find(|t| &t.name == n)
            .cloned()
            .map(Taken::Tmux),
        Target::Claude(id) => entries
            .iterm
            .iter()
            .find(|c| &c.session_id == id)
            .cloned()
            .map(Taken::Claude),
    }
    .ok_or_else(|| Error::exit(EXIT_NOTHING, format!("no such {} session", pool.noun())))?;
    let live: Vec<String> = crate::core::tmux::list_sessions()
        .map_err(|e| Error::exit(127, e.to_string()))?
        .into_iter()
        .map(|t| t.name)
        .collect();
    let skip: HashSet<String> = discovery::discover()
        .into_iter()
        .filter_map(|r| r.session_id)
        .collect();
    // A closed Claude session whose tmux session is still there: a new window in it.
    let host_session = match (&entry, pool) {
        (Taken::Claude(c), Pool::Closed) => c
            .tmux_session
            .clone()
            .filter(|h| live.iter().any(|l| l == h)),
        _ => None,
    };
    let (kind, from, layout) = match &entry {
        Taken::Tmux(t) => ("tmux", t.name.clone(), t.clone()),
        Taken::Claude(c) => ("claude", c.label(), as_tmux(c, &live)),
    };
    let name = match (&entry, &host_session) {
        (_, Some(h)) => h.clone(),
        (Taken::Tmux(t), None) => restore_name(&t.name, &live),
        (Taken::Claude(_), None) => layout.name.clone(),
    };
    let taken = if dry {
        None
    } else {
        Some(take(pool, target)??)
    };
    let mut ex = Exec {
        dry,
        log: Vec::new(),
    };
    let mut warnings = Vec::new();
    let built = match (&entry, &host_session) {
        (Taken::Claude(c), Some(h)) => {
            reopen_in(&mut ex, c, h, launcher, &skip, &mut warnings).map(|l| (l, 1, 1, false))
        }
        _ => rebuild(&mut ex, &layout, &name, launcher, &skip, &mut warnings).map(|l| {
            (
                l,
                layout.windows.len(),
                layout.pane_count(),
                name != from || kind == "claude",
            )
        }),
    };
    match built {
        Ok((launched, windows, panes, renamed)) => Ok(Restored {
            kind,
            from: from.clone(),
            renamed,
            session: name,
            windows,
            panes,
            launched,
            warnings,
            commands: ex.log,
            dry_run: dry,
        }),
        Err(e) => {
            if let Some(t) = taken {
                put_back(pool, t);
            }
            Err(e)
        }
    }
}

/// A Claude session resumed in a new window of the live tmux session `session`.
fn reopen_in(
    ex: &mut Exec,
    c: &ClaudeSnap,
    session: &str,
    launcher: &str,
    skip: &HashSet<String>,
    warnings: &mut Vec<String>,
) -> Result<Vec<Launched>> {
    if skip.contains(&c.session_id) {
        warnings.push(format!(
            "{} is already running elsewhere — not started again",
            c.label()
        ));
        return Ok(Vec::new());
    }
    let dir = usable_dir(c.cwd.as_deref().unwrap_or("~"), warnings);
    let pane = ex.run(
        vec![
            s("new-window"),
            s("-d"),
            s("-t"),
            format!("={session}:"),
            s("-e"),
            s(crate::core::backend::SPAWN_SHELL_ENV),
            s("-c"),
            dir.clone(),
            s("-P"),
            s("-F"),
            s("#{pane_id}"),
        ],
        "%new",
    )?;
    let line = resume_line(launcher, &dir, c);
    ex.type_line(&pane, &line)?;
    Ok(vec![Launched {
        session_id: c.session_id.clone(),
        title: c.title.clone(),
        line,
    }])
}

/// Settle a pending reboot in `snap` (so "forget all" also covers what the old boot ran).
/// → whether it did.
fn settle(snap: &mut Snapshot, boot: Option<&str>) -> bool {
    let rebooted = matches!((snap.boot_id.as_deref(), boot), (Some(a), Some(b)) if a != b);
    if rebooted {
        let host = snap.host.clone();
        *snap = step(
            std::mem::take(snap),
            Live::default(),
            boot,
            &host,
            Utc::now(),
            keep_closed_secs(),
        );
    }
    rebooted
}

/// Drop dormant entries without restoring them: one `target`, or all with `None`.
/// → the names of what was forgotten.
pub fn forget(target: Option<&Target>) -> Result<Vec<String>> {
    if crate::core::hosts::dry_run() {
        let s = current()?;
        return Ok(match target {
            Some(t) => vec![t.label(&s)],
            None => dormant_views(&s).into_iter().map(|v| v.name).collect(),
        });
    }
    let boot = boot_id();
    update(|snap| {
        let rebooted = settle(snap, boot.as_deref());
        let mut gone = Vec::new();
        match target {
            None => {
                gone.extend(dormant_views(snap).into_iter().map(|v| v.name));
                snap.dormant.tmux.clear();
                snap.dormant.iterm.clear();
            }
            Some(t) => {
                gone.push(t.label(snap));
                match t {
                    Target::Tmux(n) => snap.dormant.tmux.retain(|d| &d.name != n),
                    Target::Claude(id) => snap.dormant.iterm.retain(|d| &d.session_id != id),
                }
            }
        }
        Ok((rebooted || !gone.is_empty(), gone))
    })
}

/// Drop recently closed entries: one `target`, or all with `None` (what still lingers too —
/// a reboot within the grace period will not make it dormant any more).
/// → the names of what was forgotten.
pub fn forget_closed(target: Option<&Target>) -> Result<Vec<String>> {
    if crate::core::hosts::dry_run() {
        let s = current()?;
        return Ok(match target {
            Some(t) => vec![t.label_in(&closed_pool(&s))],
            None => closed_views(&s).into_iter().map(|v| v.view.name).collect(),
        });
    }
    let boot = boot_id();
    update(|snap| {
        let rebooted = settle(snap, boot.as_deref());
        let mut gone = Vec::new();
        match target {
            None => {
                gone.extend(closed_views(snap).into_iter().map(|v| v.view.name));
                forget_all_closed(snap);
            }
            Some(t) => {
                let label = t.label_in(&closed_pool(snap));
                if remove_closed(snap, t) {
                    gone.push(label);
                }
            }
        }
        Ok((rebooted || !gone.is_empty(), gone))
    })
}

/// Empty the recently closed list, lingering entries included.
fn forget_all_closed(snap: &mut Snapshot) {
    snap.closed.tmux.clear();
    snap.closed.claude.clear();
    snap.tmux.retain(|t| t.gone_at.is_none());
    snap.iterm.retain(|c| c.gone_at.is_none());
    for p in snap
        .tmux
        .iter_mut()
        .flat_map(|t| t.windows.iter_mut())
        .flat_map(|w| w.panes.iter_mut())
    {
        if p.claude.as_ref().is_some_and(|c| c.gone_at.is_some()) {
            p.claude = None;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn argv(s: &str) -> Vec<String> {
        s.split_whitespace().map(str::to_string).collect()
    }

    fn at(secs: i64) -> DateTime<Utc> {
        DateTime::from_timestamp(1_800_000_000 + secs, 0).unwrap()
    }

    fn claude(id: &str) -> ClaudeSnap {
        ClaudeSnap {
            session_id: id.into(),
            title: Some(format!("work {id}")),
            cwd: Some("/tmp".into()),
            ..Default::default()
        }
    }

    fn tmux(name: &str, ids: &[&str]) -> TmuxSnap {
        let mut panes = vec![PaneSnap {
            index: 0,
            cwd: "/tmp".into(),
            active: true,
            ..Default::default()
        }];
        for (i, id) in ids.iter().enumerate() {
            if i > 0 {
                panes.push(PaneSnap {
                    index: i as i64,
                    cwd: "/tmp".into(),
                    ..Default::default()
                });
            }
            panes[i].claude = Some(claude(id));
        }
        TmuxSnap {
            name: name.into(),
            windows: vec![WindowSnap {
                index: 1,
                name: "zsh".into(),
                layout: "b25d,120x40,0,0,1".into(),
                active: true,
                auto_name: true,
                panes,
                ..Default::default()
            }],
            ..Default::default()
        }
    }

    fn live(t: Vec<TmuxSnap>, i: Vec<ClaudeSnap>) -> Live {
        Live { tmux: t, iterm: i }
    }

    #[test]
    fn only_replay_safe_flags_survive() {
        let a = argv(
            "--dangerously-skip-permissions --model opus -n my-name --resume abc \
             --permission-mode=plan --add-dir /a --add-dir=/b -p fix the bug --chrome \
             --session-id x --agent rev --fallback-model sonnet -c",
        );
        assert_eq!(
            replayable_flags(&a),
            argv(
                "--dangerously-skip-permissions --model opus --permission-mode plan \
                 --add-dir /a --add-dir /b --chrome --agent rev --fallback-model sonnet"
            )
        );
        // A prompt after the flags is never a value; a value-less flag is dropped.
        assert_eq!(
            replayable_flags(&argv("--model claude-opus-5-5 please fix it")),
            argv("--model claude-opus-5-5")
        );
        assert_eq!(
            replayable_flags(&argv("--model --chrome")),
            argv("--chrome")
        );
        // The last of a repeated flag is the one claude used; --add-dir repeats.
        assert_eq!(
            replayable_flags(&argv("--model a --model b --add-dir /x --add-dir /x")),
            argv("--model b --add-dir /x")
        );
    }

    #[test]
    fn the_launcher_flags_are_not_passed_twice() {
        let c = ClaudeSnap {
            session_id: "abcd-1234".into(),
            flags: argv("--dangerously-skip-permissions --model opus --add-dir /a"),
            ..Default::default()
        };
        assert_eq!(
            resume_line("claude", "/w/app", &c),
            "cd '/w/app' && claude --dangerously-skip-permissions --model opus --add-dir /a --resume=abcd-1234"
        );
        assert_eq!(
            resume_line("cl --dangerously-skip-permissions --model=x", "/w", &c),
            "cd '/w' && cl --dangerously-skip-permissions --model=x --add-dir /a --resume=abcd-1234"
        );
    }

    #[test]
    fn boot_time_parses() {
        assert_eq!(
            parse_boottime("{ sec = 1727000000, usec = 315387 } Tue Sep  1 18:48:59 2026\n")
                .as_deref(),
            Some("1727000000")
        );
        assert_eq!(parse_boottime("garbage"), None);
    }

    #[test]
    fn panes_group_into_sessions_and_windows() {
        let text = "\
2\t0\t1\t0\t1\t%5\tlay2\t/b\tapi\tzsh
1\t1\t0\t1\t0\t%4\tlay1\t/a2\tapi\tedit\twith tab
1\t1\t0\t0\t1\t%3\tlay1\t/a\tapi\tedit\twith tab
0\t1\t1\t0\t1\t%9\tl\t/c\tweb\tnode
garbage";
        let (s, at) = build_layout(text);
        assert_eq!(s.len(), 2);
        assert_eq!(s[0].name, "api");
        let w: Vec<i64> = s[0].windows.iter().map(|w| w.index).collect();
        assert_eq!(w, [1, 2], "sorted by index");
        assert_eq!(s[0].windows[0].name, "edit\twith tab");
        assert!(!s[0].windows[0].auto_name);
        assert_eq!(s[0].windows[0].panes[0].cwd, "/a");
        assert_eq!(at["%4"], (0, 0, 1));
        assert_eq!(at["%5"], (0, 1, 0));
        assert_eq!(at["%9"], (1, 0, 0));
    }

    #[test]
    fn discovered_sessions_land_in_their_panes() {
        let (layout, pos) = build_layout("0\t1\t1\t0\t1\t%1\tl\t/a\tjob\tzsh\n");
        let rows = vec![
            Session {
                pid: 10,
                session_id: Some("s-tmux".into()),
                backend: Backend::Tmux,
                handle: Some("%1".into()),
                ..Default::default()
            },
            Session {
                pid: 11,
                session_id: Some("s-iterm".into()),
                backend: Backend::Iterm,
                handle: Some("GUID".into()),
                ..Default::default()
            },
            // A pane on a server fleet cannot see is recorded on its own.
            Session {
                pid: 12,
                session_id: Some("s-other".into()),
                backend: Backend::Tmux,
                handle: Some("%77".into()),
                ..Default::default()
            },
            Session {
                pid: 13,
                session_id: None,
                ..Default::default()
            },
        ];
        let argv = HashMap::from([(10, argv("claude --chrome hello"))]);
        let l = place(&rows, layout, &pos, &argv);
        let c = l.tmux[0].windows[0].panes[0].claude.as_ref().unwrap();
        assert_eq!(c.session_id, "s-tmux");
        assert_eq!(c.flags, ["--chrome"]);
        let lone: Vec<&str> = l.iterm.iter().map(|c| c.session_id.as_str()).collect();
        assert_eq!(lone, ["s-iterm", "s-other"]);
    }

    #[test]
    fn same_boot_records_the_live_state_and_nothing_goes_dormant() {
        let s = transition(
            Snapshot::default(),
            live(vec![tmux("api", &["a1"])], vec![claude("i1")]),
            Some("boot1"),
            "laptop",
            at(0),
        );
        assert_eq!(s.boot_id.as_deref(), Some("boot1"));
        assert_eq!(s.host, "laptop");
        assert_eq!(s.tmux.len(), 1);
        assert_eq!(s.iterm.len(), 1);
        assert!(s.dormant.is_empty());
        // Closed on purpose: lingers for the grace period, then it's gone — never dormant.
        let s = transition(s, Live::default(), Some("boot1"), "laptop", at(10));
        assert_eq!(s.tmux[0].gone_at.as_deref(), Some(iso(at(10)).as_str()));
        assert_eq!(s.iterm[0].gone_at.as_deref(), Some(iso(at(10)).as_str()));
        let s = transition(s, Live::default(), Some("boot1"), "laptop", at(100));
        assert_eq!(s.tmux.len(), 1, "still within the grace period");
        let s = transition(
            s,
            Live::default(),
            Some("boot1"),
            "laptop",
            at(10 + GRACE_SECS + 1),
        );
        assert!(s.tmux.is_empty() && s.iterm.is_empty());
        assert!(s.dormant.is_empty());
    }

    #[test]
    fn a_new_boot_makes_the_old_one_dormant() {
        let mut s = transition(
            Snapshot::default(),
            live(
                vec![tmux("api", &["a1", "a2"]), tmux("web", &[])],
                vec![claude("i1")],
            ),
            Some("boot1"),
            "laptop",
            at(0),
        );
        s.updated_at = Some(iso(at(5)));
        let s = transition(
            s,
            live(vec![tmux("fresh", &[])], vec![]),
            Some("boot2"),
            "laptop",
            at(500),
        );
        assert_eq!(s.boot_id.as_deref(), Some("boot2"));
        let names: Vec<&str> = s.dormant.tmux.iter().map(|t| t.name.as_str()).collect();
        assert_eq!(names, ["api", "web"]);
        assert_eq!(
            s.dormant.tmux[0].since.as_deref(),
            Some(iso(at(5)).as_str())
        );
        assert_eq!(s.dormant.iterm[0].session_id, "i1");
        assert_eq!(s.tmux[0].name, "fresh");
        assert_eq!(
            dormant_ids(&s),
            HashSet::from(["a1".to_string(), "a2".into(), "i1".into()])
        );
        // A session that vanished just before the reboot is dormant too.
        let mut s2 = transition(
            Snapshot::default(),
            live(vec![], vec![claude("x")]),
            Some("b1"),
            "h",
            at(0),
        );
        s2 = transition(s2, Live::default(), Some("b1"), "h", at(30));
        let s2 = transition(s2, Live::default(), Some("b2"), "h", at(90));
        assert_eq!(s2.dormant.iterm[0].session_id, "x");
        assert_eq!(s2.dormant.iterm[0].gone_at, None);
    }

    #[test]
    fn an_unknown_boot_never_marks_anything_dormant() {
        let s = transition(
            Snapshot::default(),
            live(vec![tmux("api", &["a1"])], vec![]),
            None,
            "h",
            at(0),
        );
        let s = transition(s, live(vec![], vec![]), Some("b2"), "h", at(10));
        assert!(s.dormant.is_empty());
        let s = transition(s, live(vec![tmux("x", &[])], vec![]), None, "h", at(20));
        assert_eq!(
            s.boot_id.as_deref(),
            Some("b2"),
            "an unreadable boot id keeps the last"
        );
        assert!(s.dormant.is_empty());
    }

    #[test]
    fn a_session_resumed_by_hand_is_no_longer_dormant() {
        let s = transition(
            Snapshot::default(),
            live(
                vec![tmux("api", &["a1", "a2"]), tmux("web", &[])],
                vec![claude("i1")],
            ),
            Some("b1"),
            "h",
            at(0),
        );
        let s = transition(s, Live::default(), Some("b2"), "h", at(10));
        assert_eq!(s.dormant.len(), 3);
        // `claude --resume i1` and `--resume a1` by hand, and a fresh `web` shell.
        let s = transition(
            s,
            live(vec![tmux("web", &[])], vec![claude("i1"), claude("a1")]),
            Some("b2"),
            "h",
            at(20),
        );
        assert!(s.dormant.iterm.is_empty());
        let names: Vec<&str> = s.dormant.tmux.iter().map(|t| t.name.as_str()).collect();
        assert_eq!(
            names,
            ["api"],
            "web is live again and had nothing dormant in it"
        );
        let ids: Vec<&str> = s.dormant.tmux[0]
            .claudes()
            .map(|c| c.session_id.as_str())
            .collect();
        assert_eq!(ids, ["a2"], "a1 runs elsewhere now — never started twice");
        // A live session named `api` with a2 still dormant keeps the entry.
        let s = transition(
            s,
            live(vec![tmux("api", &[])], vec![]),
            Some("b2"),
            "h",
            at(30),
        );
        assert_eq!(s.dormant.tmux.len(), 1);
        let s = transition(
            s,
            live(vec![tmux("api", &[])], vec![claude("a2")]),
            Some("b2"),
            "h",
            at(40),
        );
        assert!(s.dormant.tmux.is_empty());
    }

    #[test]
    fn a_claude_gone_from_a_live_pane_lingers_in_it() {
        let s = transition(
            Snapshot::default(),
            live(vec![tmux("api", &["a1"])], vec![]),
            Some("b1"),
            "h",
            at(0),
        );
        // The pane is still there, its claude exited (a restart quitting it, or /exit).
        let s = transition(
            s,
            live(vec![tmux("api", &[])], vec![]),
            Some("b1"),
            "h",
            at(10),
        );
        let c = s.tmux[0].windows[0].panes[0].claude.as_ref().unwrap();
        assert_eq!(c.session_id, "a1");
        assert!(c.gone_at.is_some());
        let s = transition(
            s,
            live(vec![tmux("api", &[])], vec![]),
            Some("b1"),
            "h",
            at(10 + GRACE_SECS + 1),
        );
        assert!(s.tmux[0].windows[0].panes[0].claude.is_none());
    }

    #[test]
    fn the_snapshot_round_trips_with_unknown_keys() {
        let text = r#"{
          "version": 1, "host": "laptop", "bootId": "1", "updatedAt": "2026-01-01T00:00:00Z",
          "future": { "x": 1 },
          "tmux": [ { "name": "api", "extraT": true, "windows": [ { "index": 0, "name": "zsh",
             "layout": "l", "active": true, "panes": [ { "index": 0, "cwd": "/a", "active": true,
             "claude": { "sessionId": "s1", "flags": ["--chrome"], "newKey": 2 } } ] } ] } ],
          "iterm": [],
          "dormant": { "tmux": [], "iterm": [], "other": [] }
        }"#;
        let s: Snapshot = serde_json::from_str(text).unwrap();
        let back = serde_json::to_value(&s).unwrap();
        assert_eq!(back["future"]["x"], 1);
        assert_eq!(back["tmux"][0]["extraT"], true);
        assert_eq!(
            back["tmux"][0]["windows"][0]["panes"][0]["claude"]["newKey"],
            2
        );
        assert_eq!(back["dormant"]["other"], serde_json::json!([]));
        assert!(
            back["tmux"][0].get("since").is_none(),
            "absent stays absent"
        );
        let s2: Snapshot = serde_json::from_value(back).unwrap();
        assert_eq!(s, s2);
        // And through a transition.
        let t = transition(s, Live::default(), Some("1"), "laptop", at(0));
        assert_eq!(serde_json::to_value(&t).unwrap()["future"]["x"], 1);
    }

    #[test]
    fn same_content_ignores_the_timestamp() {
        let a = Snapshot {
            updated_at: Some("x".into()),
            ..Default::default()
        };
        let b = Snapshot {
            updated_at: Some("y".into()),
            ..Default::default()
        };
        assert!(a.same_content(&b));
    }

    fn dormant_snap() -> Snapshot {
        let mut s = Snapshot::default();
        s.dormant.tmux = vec![
            tmux("api-server", &["aaaa1111-x"]),
            tmux("api-client", &[]),
            tmux("docs", &["bbbb2222-y"]),
        ];
        let mut lone = claude("cccc3333-z");
        lone.title = Some("Fix the login page".into());
        s.dormant.iterm = vec![lone];
        s
    }

    #[test]
    fn targets_resolve_by_name_then_id_then_title() {
        let s = dormant_snap();
        let r = |q: &str| resolve(&s, q);
        assert_eq!(r("docs").unwrap(), Target::Tmux("docs".into()));
        assert_eq!(r("api-s").unwrap(), Target::Tmux("api-server".into()));
        // An id inside a dormant tmux pane restores that tmux session.
        assert_eq!(r("bbbb").unwrap(), Target::Tmux("docs".into()));
        assert_eq!(
            r("cccc3333-z").unwrap(),
            Target::Claude("cccc3333-z".into())
        );
        assert_eq!(r("login").unwrap(), Target::Claude("cccc3333-z".into()));
        // Too short to be an id, and ambiguous / unknown come back as exit codes.
        let e = r("api").unwrap_err();
        assert_eq!(e.code(), EXIT_AMBIGUOUS);
        assert!(e.to_string().contains("api-server"), "{e}");
        assert_eq!(r("nothing-like-it").unwrap_err().code(), EXIT_NOTHING);
        assert_eq!(r("ccc").unwrap_err().code(), EXIT_NOTHING);
    }

    #[test]
    fn views_list_tmux_then_lone_sessions() {
        let v = dormant_views(&dormant_snap());
        assert_eq!(v.len(), 4);
        assert_eq!(v[3].kind, "claude");
        assert_eq!(v[3].target, "cccc3333-z");
        assert_eq!(v[3].name, "Fix the login page");
        let api = v.iter().find(|d| d.name == "api-server").unwrap();
        assert_eq!((api.windows, api.panes), (1, 1));
        assert_eq!(api.sessions[0].session_id, "aaaa1111-x");
        let j = serde_json::to_value(api).unwrap();
        assert!(j.get("sessionId").is_none());
        assert_eq!(j["sessions"][0]["sessionId"], "aaaa1111-x");
    }

    #[test]
    fn a_taken_name_restores_alongside() {
        assert_eq!(restore_name("api", &[]), "api");
        let live = vec!["api".to_string(), "api-restored".into()];
        assert_eq!(restore_name("api", &live), "api-restored-2");
        assert_eq!(layout_size("b25d,120x40,0,0,1"), Some((120, 40)));
        assert_eq!(layout_size("b25d,120x40,0,0{60x40,0,0,1}"), Some((120, 40)));
        assert_eq!(layout_size(""), None);
    }

    #[test]
    fn a_dry_rebuild_renders_every_command_in_order() {
        let mut t = tmux("api", &["s-1", "s-2"]);
        t.windows[0].panes.push(PaneSnap {
            index: 2,
            cwd: "/tmp".into(),
            ..Default::default()
        });
        t.windows.push(WindowSnap {
            index: 2,
            name: "logs".into(),
            layout: "l2".into(),
            auto_name: false,
            active: false,
            panes: vec![PaneSnap {
                index: 0,
                cwd: "/does/not/exist/anywhere".into(),
                ..Default::default()
            }],
            ..Default::default()
        });
        let mut ex = Exec {
            dry: true,
            log: Vec::new(),
        };
        let mut warn = Vec::new();
        let skip = HashSet::from(["s-2".to_string()]);
        let launched = rebuild(&mut ex, &t, "api", "claude", &skip, &mut warn).unwrap();
        let log: Vec<String> = ex
            .log
            .iter()
            .map(|l| l.split_once(' ').unwrap().1.to_string())
            .collect();
        assert_eq!(
            log[0],
            "new-session -d -s api -e DISABLE_AUTO_UPDATE=true -x 120 -y 40 -c /tmp -P -F '#{pane_id}'"
        );
        assert!(
            log[1].starts_with("split-window -d -t %1.0 -c /tmp"),
            "{log:?}"
        );
        assert_eq!(log[2], "select-layout -t %1.0 tiled");
        assert!(log[3].starts_with("split-window -d -t %1.1"), "{log:?}");
        assert_eq!(log[4], "select-layout -t %1.0 b25d,120x40,0,0,1");
        assert_eq!(log[5], "select-pane -t %1.0");
        assert!(
            log[6].starts_with("new-window -d -t '=api:' -n logs -c "),
            "{log:?}"
        );
        assert_eq!(log[7], "select-window -t %1.0");
        assert_eq!(
            log[8],
            "send-keys -t %1.0 -l 'cd '\\''/tmp'\\'' && claude --resume=s-1'"
        );
        assert_eq!(log[9], "send-keys -t %1.0 Enter");
        assert_eq!(log.len(), 10, "s-2 runs elsewhere: no second launch");
        assert_eq!(launched.len(), 1);
        assert!(
            warn.iter().any(|w| w.contains("no longer exists")),
            "{warn:?}"
        );
        assert!(
            warn.iter().any(|w| w.contains("already running")),
            "{warn:?}"
        );
    }

    #[test]
    fn a_lone_session_gets_a_tmux_session_named_after_it() {
        let mut c = claude("abc");
        c.title = Some("Fix: the login page!".into());
        let t = as_tmux(&c, &["Fix-the-login-page".to_string()]);
        assert_eq!(t.name, "Fix-the-login-page-2");
        assert_eq!(t.pane_count(), 1);
        c.title = Some("čćž".into());
        c.name = None;
        assert_eq!(as_tmux(&c, &[]).name, "claude");
    }

    const WEEK: i64 = 7 * 86_400;

    fn ids_of(v: &[ClosedView]) -> Vec<String> {
        v.iter().map(|c| c.view.target.clone()).collect()
    }

    #[test]
    fn a_session_closed_within_a_boot_is_closed_at_once_and_after_the_grace_period() {
        let s = step(
            Snapshot::default(),
            live(
                vec![tmux("api", &["a1"]), tmux("shell", &[])],
                vec![claude("i1")],
            ),
            Some("b1"),
            "h",
            at(0),
            WEEK,
        );
        assert!(closed_views(&s).is_empty());
        // Closed: shown at once, while it still lingers for a reboot.
        let s = step(s, Live::default(), Some("b1"), "h", at(10), WEEK);
        let v = closed_views(&s);
        assert_eq!(ids_of(&v), ["api", "i1"], "{v:?}");
        assert_eq!(v[0].closed_at.as_deref(), Some(iso(at(10)).as_str()));
        assert_eq!(v[0].view.since, v[0].closed_at);
        assert!(s.closed.is_empty(), "still lingering, not yet moved");
        // After the grace period it moves into `closed`, same closedAt; a plain shell
        // session (no Claude pane) is not kept.
        let s = step(
            s,
            Live::default(),
            Some("b1"),
            "h",
            at(10 + GRACE_SECS + 1),
            WEEK,
        );
        assert!(s.tmux.is_empty() && s.iterm.is_empty());
        assert_eq!(s.closed.tmux.len(), 1);
        assert_eq!(s.closed.tmux[0].name, "api");
        assert_eq!(s.closed.claude[0].session_id, "i1");
        assert_eq!(s.closed.claude[0].gone_at, None);
        let v = closed_views(&s);
        assert_eq!(ids_of(&v), ["api", "i1"]);
        assert_eq!(v[1].closed_at.as_deref(), Some(iso(at(10)).as_str()));
        // Never dormant, never a dormant id, never a dormant target.
        assert!(s.dormant.is_empty());
        assert!(dormant_ids(&s).is_empty());
        assert!(dormant_views(&s).is_empty());
        assert_eq!(resolve(&s, "api").unwrap_err().code(), EXIT_NOTHING);
        assert_eq!(
            resolve_closed(&s, "api").unwrap(),
            Target::Tmux("api".into())
        );
        assert_eq!(
            resolve_closed(&s, "work i1").unwrap(),
            Target::Claude("i1".into())
        );
        // A reboot later leaves it closed (expiry still applies), and nothing dormant.
        let s = step(s, Live::default(), Some("b2"), "h", at(1000), WEEK);
        assert!(s.dormant.is_empty());
        assert_eq!(closed_views(&s).len(), 2);
    }

    #[test]
    fn a_reboot_within_the_grace_period_makes_it_dormant_not_closed() {
        let s = step(
            Snapshot::default(),
            live(vec![tmux("api", &["a1"])], vec![claude("i1")]),
            Some("b1"),
            "h",
            at(0),
            WEEK,
        );
        let s = step(s, Live::default(), Some("b1"), "h", at(30), WEEK);
        assert_eq!(closed_views(&s).len(), 2);
        let s = step(s, Live::default(), Some("b2"), "h", at(90), WEEK);
        assert!(closed_views(&s).is_empty(), "{:?}", closed_views(&s));
        assert_eq!(dormant_views(&s).len(), 2);
        assert_eq!(
            dormant_ids(&s),
            HashSet::from(["a1".to_string(), "i1".into()])
        );
    }

    #[test]
    fn closed_entries_expire_and_are_capped() {
        let mut s = Snapshot {
            boot_id: Some("b".into()),
            ..Default::default()
        };
        for i in 0..60 {
            s.closed.claude.push(ClaudeSnap {
                closed_at: Some(iso(at(i))),
                ..claude(&format!("c{i:02}"))
            });
        }
        s.closed.claude.push(ClaudeSnap {
            closed_at: Some("not a time".into()),
            ..claude("bad")
        });
        let t = step(s.clone(), Live::default(), Some("b"), "h", at(100), WEEK);
        assert_eq!(t.closed.len(), MAX_CLOSED);
        assert!(
            t.closed
                .claude
                .iter()
                .all(|c| c.session_id.as_str() >= "c10")
        );
        // Older than keep: gone.
        let t = step(
            s.clone(),
            Live::default(),
            Some("b"),
            "h",
            at(50 + WEEK),
            WEEK,
        );
        let left: Vec<&str> = t
            .closed
            .claude
            .iter()
            .map(|c| c.session_id.as_str())
            .collect();
        assert_eq!(
            left,
            [
                "c50", "c51", "c52", "c53", "c54", "c55", "c56", "c57", "c58", "c59"
            ]
        );
        // keepClosedDays 0: the feature is off, nothing is kept.
        let t = step(s, Live::default(), Some("b"), "h", at(100), 0);
        assert!(t.closed.is_empty());
    }

    #[test]
    fn a_closed_session_live_again_leaves_the_list() {
        let mut s = Snapshot {
            boot_id: Some("b".into()),
            ..Default::default()
        };
        s.closed.claude.push(ClaudeSnap {
            closed_at: Some(iso(at(0))),
            ..claude("c1")
        });
        let mut t = tmux("api", &["a1", "a2"]);
        t.closed_at = Some(iso(at(0)));
        s.closed.tmux.push(t);
        // c1 and a1 resumed by hand somewhere.
        let s = step(
            s,
            live(vec![], vec![claude("c1"), claude("a1")]),
            Some("b"),
            "h",
            at(10),
            WEEK,
        );
        assert!(s.closed.claude.is_empty());
        let ids: Vec<&str> = s.closed.tmux[0]
            .claudes()
            .map(|c| c.session_id.as_str())
            .collect();
        assert_eq!(ids, ["a2"]);
        let s = step(
            s,
            live(vec![], vec![claude("a2")]),
            Some("b"),
            "h",
            at(20),
            WEEK,
        );
        assert!(s.closed.is_empty(), "no Claude pane left: gone");
        // And an id that is dormant is never closed too.
        let mut s2 = Snapshot {
            boot_id: Some("b".into()),
            ..Default::default()
        };
        s2.closed.claude.push(ClaudeSnap {
            closed_at: Some(iso(at(0))),
            ..claude("x")
        });
        s2.dormant.iterm.push(claude("x"));
        let s2 = step(s2, Live::default(), Some("b"), "h", at(10), WEEK);
        assert!(s2.closed.is_empty());
        assert_eq!(s2.dormant.len(), 1);
    }

    #[test]
    fn a_claude_that_exits_in_a_live_pane_is_closed_with_its_tmux_session() {
        let s = step(
            Snapshot::default(),
            live(vec![tmux("api", &["a1"])], vec![]),
            Some("b"),
            "h",
            at(0),
            WEEK,
        );
        // /exit: the shell stays.
        let s = step(
            s,
            live(vec![tmux("api", &[])], vec![]),
            Some("b"),
            "h",
            at(10),
            WEEK,
        );
        let v = closed_views(&s);
        assert_eq!(v.len(), 1);
        assert_eq!(v[0].view.kind, "claude");
        assert_eq!(v[0].view.target, "a1");
        assert_eq!(v[0].tmux_session.as_deref(), Some("api"));
        let j = serde_json::to_value(&v[0]).unwrap();
        assert_eq!(j["tmuxSession"], "api");
        assert_eq!(j["closedAt"], iso(at(10)));
        assert_eq!(j["kind"], "claude");
        let s = step(
            s,
            live(vec![tmux("api", &[])], vec![]),
            Some("b"),
            "h",
            at(10 + GRACE_SECS + 1),
            WEEK,
        );
        assert!(s.tmux[0].windows[0].panes[0].claude.is_none());
        assert_eq!(s.closed.claude[0].tmux_session.as_deref(), Some("api"));
        assert_eq!(closed_views(&s)[0].tmux_session.as_deref(), Some("api"));
        // Its closing tmux session later: a newer entry; the id stays in one place.
        let mut s = s;
        let t = TmuxSnap {
            closed_at: Some(iso(at(2000))),
            ..tmux("api", &["a1"])
        };
        s.closed.add_tmux(t);
        assert!(s.closed.claude.is_empty());
        assert_eq!(s.closed.tmux.len(), 1);
    }

    #[test]
    fn closed_entries_are_taken_and_forgotten_wherever_they_are_kept() {
        let s = step(
            Snapshot::default(),
            live(
                vec![tmux("api", &["a1"]), tmux("web", &["w1"])],
                vec![claude("i1"), claude("i2")],
            ),
            Some("b"),
            "h",
            at(0),
            WEEK,
        );
        // web closed, a1 exited in its pane, i1 closed — all still lingering — and i2 long closed.
        let mut s = step(
            s,
            live(vec![tmux("api", &[])], vec![claude("i2")]),
            Some("b"),
            "h",
            at(10),
            WEEK,
        );
        s.iterm.retain(|c| c.session_id != "i2");
        s.closed.claude.push(ClaudeSnap {
            closed_at: Some(iso(at(5))),
            ..claude("i2")
        });
        assert_eq!(closed_views(&s).len(), 4);
        let mut t = s.clone();
        assert!(remove_closed(&mut t, &Target::Tmux("web".into())));
        assert!(remove_closed(&mut t, &Target::Claude("a1".into())));
        assert!(remove_closed(&mut t, &Target::Claude("i1".into())));
        assert!(remove_closed(&mut t, &Target::Claude("i2".into())));
        assert!(!remove_closed(&mut t, &Target::Claude("i2".into())));
        assert!(closed_views(&t).is_empty());
        assert_eq!(t.tmux.len(), 1, "the live api session stays");
        // Forget all: lingering ones too, so a reboot now leaves nothing dormant.
        forget_all_closed(&mut s);
        assert!(closed_views(&s).is_empty());
        let s = step(s, Live::default(), Some("b2"), "h", at(20), WEEK);
        assert_eq!(dormant_views(&s).len(), 1, "only the live api session");
    }

    #[test]
    fn a_closed_claude_reopens_in_a_window_of_its_tmux_session() {
        let mut ex = Exec {
            dry: true,
            log: Vec::new(),
        };
        let mut warn = Vec::new();
        let c = ClaudeSnap {
            flags: argv("--chrome"),
            tmux_session: Some("api".into()),
            ..claude("s-1")
        };
        let launched = reopen_in(&mut ex, &c, "api", "claude", &HashSet::new(), &mut warn).unwrap();
        let log: Vec<String> = ex
            .log
            .iter()
            .map(|l| l.split_once(' ').unwrap().1.to_string())
            .collect();
        assert_eq!(
            log[0],
            "new-window -d -t '=api:' -e DISABLE_AUTO_UPDATE=true -c /tmp -P -F '#{pane_id}'"
        );
        assert_eq!(
            log[1],
            "send-keys -t %new -l 'cd '\\''/tmp'\\'' && claude --chrome --resume=s-1'"
        );
        assert_eq!(launched.len(), 1);
        // Running elsewhere: nothing opened.
        let mut ex = Exec {
            dry: true,
            log: Vec::new(),
        };
        let skip = HashSet::from(["s-1".to_string()]);
        assert!(
            reopen_in(&mut ex, &c, "api", "claude", &skip, &mut warn)
                .unwrap()
                .is_empty()
        );
        assert!(ex.log.is_empty());
    }

    #[test]
    fn an_old_file_without_closed_loads_and_keeps_unknown_closed_keys() {
        let s: Snapshot = serde_json::from_str(
            r#"{"version":1,"host":"h","bootId":"1","tmux":[],"iterm":[],"dormant":{"tmux":[],"iterm":[]}}"#,
        )
        .unwrap();
        assert!(s.closed.is_empty());
        let s: Snapshot = serde_json::from_str(
            r#"{"closed":{"tmux":[],"claude":[{"sessionId":"x","closedAt":"2026-01-01T00:00:00Z","tmuxSession":"api","k":1}],"later":true}}"#,
        )
        .unwrap();
        let back = serde_json::to_value(&s).unwrap();
        assert_eq!(back["closed"]["later"], true);
        assert_eq!(back["closed"]["claude"][0]["k"], 1);
        assert_eq!(back["closed"]["claude"][0]["tmuxSession"], "api");
    }
}
