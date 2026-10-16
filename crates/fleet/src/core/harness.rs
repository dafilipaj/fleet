//! Coding-agent harnesses: Claude Code, Codex and pi.
//!
//! Claude Code keeps a live registry (`~/.claude/sessions/<pid>.json`, see
//! [`crate::core::discovery`]); the others don't. For them this module finds the
//! interactive processes with `ps`, matches each to the transcript it writes, and
//! reads the status, first prompt, model and context use from that transcript's tail:
//!
//! - **Codex** — `$CODEX_HOME/sessions/YYYY/MM/DD/rollout-<stamp>-<id>.jsonl` (default
//!   `~/.codex`); the first line is `session_meta` with `id` and `cwd`. A turn runs from
//!   `task_started` to `task_complete` / `turn_aborted`; an `*_approval_request` waits.
//! - **pi** — `~/.pi/agent/sessions/--<cwd with / as ->--/<stamp>_<id>.jsonl`; the first
//!   line is `{ type: "session", id, cwd }`. Busy while the last message is the user's, a
//!   tool result or an assistant turn that stopped for a tool call.
//!
//! A process is matched to a transcript by an id on its command line (`codex resume <id>`,
//! `pi --session <id>`), else by cwd and start time: the newest transcript in its cwd that
//! was created after it started, then the newest one written to since. A fresh Codex
//! session has no transcript until its first prompt — it is listed without a session id.

use std::collections::{HashMap, HashSet};
use std::io::{BufRead, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::core::context::ContextUsage;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Harness {
    #[default]
    Claude,
    Codex,
    Pi,
}

impl Harness {
    pub const ALL: [Harness; 3] = [Harness::Claude, Harness::Codex, Harness::Pi];

    /// The id used on the command line, in config and in JSON.
    pub fn id(self) -> &'static str {
        match self {
            Harness::Claude => "claude",
            Harness::Codex => "codex",
            Harness::Pi => "pi",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Harness::Claude => "Claude Code",
            Harness::Codex => "Codex",
            Harness::Pi => "pi",
        }
    }

    pub fn parse(s: &str) -> Option<Harness> {
        match s.trim().to_ascii_lowercase().as_str() {
            "claude" | "claude-code" | "cc" => Some(Harness::Claude),
            "codex" => Some(Harness::Codex),
            "pi" => Some(Harness::Pi),
            _ => None,
        }
    }

    /// The command that launches it in a spawned session. Claude: `FLEET_CMD`, then
    /// config `claude`; the others: config `harnesses.<id>.command`; else the bare name.
    pub fn command(self) -> String {
        let cfg = crate::core::config::get();
        let configured = match self {
            Harness::Claude => std::env::var("FLEET_CMD")
                .ok()
                .filter(|c| !c.trim().is_empty())
                .or_else(|| cfg.claude.clone()),
            _ => cfg.harness_command(self.id()),
        };
        configured
            .map(|c| c.trim().to_string())
            .filter(|c| !c.is_empty())
            .unwrap_or_else(|| self.id().to_string())
    }

    /// Whether the launcher takes `-n <name>` for the session's display name.
    pub fn takes_name(self) -> bool {
        matches!(self, Harness::Claude | Harness::Pi)
    }

    /// The slash command that renames a live session.
    pub fn rename_command(self, title: &str) -> String {
        match self {
            Harness::Claude | Harness::Codex => format!("/rename {title}"),
            Harness::Pi => format!("/name {title}"),
        }
    }

    /// Arguments that resume `session_id` (appended to the launcher).
    pub fn resume_args(self, session_id: &str) -> String {
        let id = crate::core::tools::shq_min(session_id);
        match self {
            Harness::Claude => format!("--resume {id}"),
            Harness::Codex => format!("resume {id}"),
            Harness::Pi => format!("--session {id}"),
        }
    }

    /// The resume command line: `<launcher> [--model m] <resume args>`.
    pub fn resume_command(self, launcher: &str, model: Option<&str>, session_id: &str) -> String {
        let mut cmd = launcher.to_string();
        if let Some(m) = model.filter(|m| !m.is_empty()) {
            cmd.push_str(" --model ");
            cmd.push_str(&crate::core::tools::shq(m));
        }
        cmd.push(' ');
        cmd.push_str(&self.resume_args(session_id));
        cmd
    }
}

impl std::fmt::Display for Harness {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.id())
    }
}

// --- models -------------------------------------------------------------------

/// One entry of a harness's model picker. `id: ""` is the harness's own default.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ModelOption {
    pub id: String,
    pub label: String,
}

fn opt(id: &str, label: &str) -> ModelOption {
    ModelOption {
        id: id.into(),
        label: label.into(),
    }
}

/// Claude's picker (the web's `web.models` overrides it there).
pub fn claude_models() -> Vec<ModelOption> {
    vec![
        opt("", "Default"),
        opt("claude-fable-5-1", "Fable 5.1"),
        opt("claude-opus-5-5", "Opus 5.5"),
        opt("claude-sonnet-5-5", "Sonnet 5.5"),
        opt("claude-haiku-5-5", "Haiku 5.5"),
    ]
}

pub fn codex_home() -> PathBuf {
    std::env::var_os("CODEX_HOME")
        .filter(|p| !p.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| dirs::home_dir().unwrap_or_default().join(".codex"))
}

pub fn pi_home() -> PathBuf {
    std::env::var_os("PI_CODING_AGENT_DIR")
        .filter(|p| !p.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| dirs::home_dir().unwrap_or_default().join(".pi/agent"))
}

/// Codex's picker: the listed models of its own cache (`models_cache.json`, in its
/// priority order), after the default — labelled with `model` from `config.toml`.
pub fn codex_models(home: &Path) -> Vec<ModelOption> {
    let default = std::fs::read_to_string(home.join("config.toml"))
        .ok()
        .and_then(|t| {
            t.lines().find_map(|l| {
                let (k, v) = l.split_once('=')?;
                (k.trim() == "model").then(|| v.trim().trim_matches('"').to_string())
            })
        });
    let mut out = vec![opt("", &default_label(default.as_deref()))];
    let cache: Value = std::fs::read_to_string(home.join("models_cache.json"))
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or(Value::Null);
    let mut models: Vec<(i64, ModelOption)> = cache
        .get("models")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter(|m| m.get("visibility").and_then(Value::as_str) == Some("list"))
        .filter_map(|m| {
            let id = m.get("slug")?.as_str()?;
            let label = m.get("display_name").and_then(Value::as_str).unwrap_or(id);
            let prio = m
                .get("priority")
                .and_then(Value::as_i64)
                .unwrap_or(i64::MAX);
            Some((prio, opt(id, label)))
        })
        .collect();
    models.sort_by_key(|(p, _)| *p);
    out.extend(models.into_iter().map(|(_, m)| m));
    out
}

/// pi's picker: `pi --list-models` (the providers it has credentials for) after the
/// default from its `settings.json`. Ids are `provider/model`.
pub fn pi_models(home: &Path, launcher: &str) -> Vec<ModelOption> {
    let settings: Value = std::fs::read_to_string(home.join("settings.json"))
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or(Value::Null);
    let default = settings
        .get("defaultModel")
        .and_then(Value::as_str)
        .map(str::to_string);
    let mut out = vec![opt("", &default_label(default.as_deref()))];
    let bin = launcher.split_whitespace().next().unwrap_or("pi");
    let bin = crate::core::tools::resolve_binary(bin, None);
    if let Some(text) = run_with_timeout(
        Command::new(bin)
            .arg("--list-models")
            .arg("--offline")
            .env("PATH", crate::core::tools::search_path()),
        Duration::from_secs(15),
    ) {
        out.extend(parse_pi_models(&text));
    }
    out
}

fn default_label(model: Option<&str>) -> String {
    match model.filter(|m| !m.is_empty()) {
        Some(m) => format!("Default ({m})"),
        None => "Default".into(),
    }
}

/// `provider  model  context …` table rows → `provider/model`.
fn parse_pi_models(text: &str) -> Vec<ModelOption> {
    text.lines()
        .filter_map(|l| {
            let mut cols = l.split_whitespace();
            let provider = cols.next()?;
            let model = cols.next()?;
            if provider == "provider" {
                return None;
            }
            Some(opt(
                &format!("{provider}/{model}"),
                &format!("{model} ({provider})"),
            ))
        })
        .collect()
}

/// stdout of `cmd`, or `None` when it fails or outlives `timeout` (then it is killed).
fn run_with_timeout(cmd: &mut Command, timeout: Duration) -> Option<String> {
    let mut child = cmd
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
        .ok()?;
    let mut stdout = child.stdout.take()?;
    let reader = std::thread::spawn(move || {
        let mut s = String::new();
        let _ = stdout.read_to_string(&mut s);
        s
    });
    let start = std::time::Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(st)) => {
                let out = reader.join().ok()?;
                return st.success().then_some(out);
            }
            Ok(None) if start.elapsed() < timeout => std::thread::sleep(Duration::from_millis(50)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
        }
    }
}

/// `fleet harness list --json`: one row per harness.
#[derive(Debug, Clone, Serialize)]
pub struct HarnessInfo {
    pub id: &'static str,
    pub label: &'static str,
    /// The launch command (`claude`, a wrapper, …).
    pub command: String,
    /// Its first word resolves to an executable (PATH, the usual install dirs).
    pub installed: bool,
    pub models: Vec<ModelOption>,
}

pub fn info(h: Harness, with_models: bool) -> HarnessInfo {
    let command = h.command();
    let bin = command.split_whitespace().next().unwrap_or(h.id());
    let installed = if bin.contains('/') {
        crate::core::tools::is_executable(Path::new(&crate::core::tools::expand_tilde(bin)))
    } else {
        crate::core::tools::find_binary(bin).is_some()
    };
    let models = match (with_models && installed, h) {
        (false, _) => Vec::new(),
        (true, Harness::Claude) => claude_models(),
        (true, Harness::Codex) => codex_models(&codex_home()),
        (true, Harness::Pi) => pi_models(&pi_home(), &command),
    };
    HarnessInfo {
        id: h.id(),
        label: h.label(),
        command,
        installed,
        models,
    }
}

/// Whether a spawn's launcher can start, asked before it is typed into a shell.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LauncherCheck {
    Found,
    Missing,
    /// Can't tell (a launcher that starts with `VAR=…`, an unusual shell, a slow profile):
    /// the spawn goes ahead, as it always did.
    Unknown,
}

/// Look for the launcher's first word: a path must be executable; a bare name is looked up
/// on `PATH` and the usual install dirs, then — since the spawned shell is a login shell whose
/// profile may add to `PATH` or define the command as a function (nvm, a telemetry wrapper) —
/// in a login shell of `$SHELL`. Only a shell that answered "not found" makes it `Missing`.
/// Everything is judged from `dir`, where the spawned shell will run the launcher: a relative
/// path (`./agent`, `bin/agent`) is resolved there, and the login shell starts there.
pub fn check_launcher(launcher: &str, dir: &Path) -> LauncherCheck {
    let Some(word) = launcher.split_whitespace().next() else {
        return LauncherCheck::Unknown;
    };
    if word.starts_with('-')
        || !word
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "._/~+-".contains(c))
    {
        return LauncherCheck::Unknown;
    }
    if word.contains('/') {
        let p = PathBuf::from(crate::core::tools::expand_tilde(word));
        // Relative to where the shell will be, not to where fleet runs.
        let p = if p.is_absolute() { p } else { dir.join(p) };
        return if crate::core::tools::is_executable(&p) {
            LauncherCheck::Found
        } else {
            LauncherCheck::Missing
        };
    }
    if crate::core::tools::find_binary(word).is_some()
        || (word == "claude" && crate::core::tools::claude().is_some())
    {
        return LauncherCheck::Found;
    }
    login_shell_has(word, dir)
}

/// `command -v <word>` in an interactive login shell of `$SHELL` (sh-family shells only).
fn login_shell_has(word: &str, dir: &Path) -> LauncherCheck {
    let shell = std::env::var("SHELL")
        .ok()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "/bin/sh".into());
    let base = Path::new(&shell)
        .file_name()
        .map(|b| b.to_string_lossy().into_owned())
        .unwrap_or_default();
    if !matches!(base.as_str(), "sh" | "bash" | "zsh" | "dash" | "ksh") {
        return LauncherCheck::Unknown;
    }
    let mut cmd = Command::new(&shell);
    cmd.args([
        "-l",
        "-i",
        "-c",
        r#"if command -v "$1" >/dev/null 2>&1; then echo fleet:found; else echo fleet:missing; fi"#,
        "fleet",
        word,
    ])
    .env("DISABLE_AUTO_UPDATE", "true")
    .current_dir(dir);
    // A profile may print too; only the line that starts with `fleet:` is ours.
    match run_with_timeout(&mut cmd, Duration::from_secs(15))
        .as_deref()
        .and_then(|out| out.lines().rev().find(|l| l.starts_with("fleet:")))
    {
        Some("fleet:found") => LauncherCheck::Found,
        Some("fleet:missing") => LauncherCheck::Missing,
        _ => LauncherCheck::Unknown,
    }
}

// --- processes ----------------------------------------------------------------

/// A live interactive Codex or pi process.
#[derive(Debug, Clone, PartialEq)]
pub struct Proc {
    pub pid: i64,
    pub tty: String,
    pub harness: Harness,
    /// Start time, ms since the epoch.
    pub start_ms: i64,
    pub argv: Vec<String>,
    pub cwd: Option<String>,
}

impl Proc {
    /// A session id named on the command line.
    pub fn explicit_session(&self) -> Option<String> {
        explicit_session(self.harness, &self.argv)
    }

    /// `--model` / `-m` from the command line.
    pub fn model_arg(&self) -> Option<String> {
        let mut it = self.argv.iter().skip(1);
        while let Some(a) = it.next() {
            if a == "--model" || (a == "-m" && self.harness == Harness::Codex) {
                return it.next().cloned();
            }
            if let Some(v) = a.strip_prefix("--model=") {
                return Some(v.to_string());
            }
        }
        None
    }
}

fn explicit_session(h: Harness, argv: &[String]) -> Option<String> {
    let args: Vec<&str> = argv.iter().skip(1).map(String::as_str).collect();
    match h {
        Harness::Codex => {
            let i = args.iter().position(|a| *a == "resume" || *a == "fork")?;
            if args[i] == "fork" {
                return None; // a fork gets a new id
            }
            args[i + 1..]
                .iter()
                .find(|a| !a.starts_with('-'))
                .map(|s| s.to_string())
        }
        Harness::Pi => {
            let mut it = args.iter();
            while let Some(a) = it.next() {
                if *a == "--session" || *a == "--session-id" {
                    return it.next().map(|s| s.to_string());
                }
            }
            None
        }
        Harness::Claude => None,
    }
}

/// Which harness an interactive process with this argv runs, if any.
/// Non-interactive runs (`codex exec`, `codex app-server`, `pi -p`, `pi --mode json`,
/// `pi install`, …) are not sessions anyone supervises.
pub fn classify(argv: &[String]) -> Option<Harness> {
    let prog = argv.first()?;
    let base = prog.rsplit('/').next().unwrap_or(prog);
    let rest: Vec<&str> = argv.iter().skip(1).map(String::as_str).collect();
    match base {
        "codex" => {
            // Subcommands come first; anything but resume/fork is not the TUI.
            let first = rest.iter().find(|a| !a.starts_with('-')).copied();
            const TUI: [&str; 2] = ["resume", "fork"];
            const OTHER: [&str; 22] = [
                "exec",
                "e",
                "review",
                "login",
                "logout",
                "mcp",
                "plugin",
                "app-server",
                "remote-control",
                "app",
                "completion",
                "update",
                "doctor",
                "sandbox",
                "debug",
                "apply",
                "a",
                "queue",
                "archive",
                "delete",
                "cloud",
                "exec-server",
            ];
            if first.is_some_and(|f| OTHER.contains(&f) || f == "help" || f == "features")
                && !first.is_some_and(|f| TUI.contains(&f))
            {
                return None;
            }
            if rest
                .iter()
                .any(|a| matches!(*a, "--version" | "-V" | "--help" | "-h"))
            {
                return None;
            }
            Some(Harness::Codex)
        }
        "pi" => {
            const OTHER: [&str; 8] = [
                "install",
                "remove",
                "uninstall",
                "update",
                "list",
                "config",
                "auth",
                "mcp",
            ];
            if rest.first().is_some_and(|f| OTHER.contains(f)) {
                return None;
            }
            let mut it = rest.iter();
            while let Some(a) = it.next() {
                match *a {
                    "-p" | "--print" | "--list-models" | "--export" | "--help" | "-h"
                    | "--version" | "-v" => return None,
                    "--mode" if it.next().is_some_and(|m| *m != "text") => return None,
                    _ => {}
                }
            }
            Some(Harness::Pi)
        }
        _ => None,
    }
}

/// `[[dd-]hh:]mm:ss` (ps `etime`) → seconds.
fn parse_etime(s: &str) -> Option<i64> {
    let (days, rest) = match s.split_once('-') {
        Some((d, r)) => (d.parse::<i64>().ok()?, r),
        None => (0, s),
    };
    let parts: Vec<i64> = rest
        .split(':')
        .map(|p| p.parse().ok())
        .collect::<Option<_>>()?;
    let secs = match parts.as_slice() {
        [m, s] => m * 60 + s,
        [h, m, s] => h * 3600 + m * 60 + s,
        _ => return None,
    };
    Some(days * 86400 + secs)
}

/// One `ps` line (`pid etime tty args…`) → a harness process (cwd not filled).
fn parse_ps_line(line: &str, now_ms: i64) -> Option<Proc> {
    let mut cols = line.split_whitespace();
    let pid = cols.next()?.parse().ok()?;
    let etime = parse_etime(cols.next()?)?;
    let tty = cols.next()?;
    if tty == "??" || tty == "?" || tty == "-" {
        return None;
    }
    let argv: Vec<String> = cols.map(str::to_string).collect();
    let harness = classify(&argv)?;
    Some(Proc {
        pid,
        tty: tty.to_string(),
        harness,
        start_ms: now_ms - etime * 1000,
        argv,
        cwd: None,
    })
}

fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

/// Every live interactive Codex / pi process with a terminal.
pub fn processes() -> Vec<Proc> {
    let Ok(out) = Command::new("ps")
        .args(["-axww", "-o", "pid=,etime=,tty=,args="])
        .output()
    else {
        return Vec::new();
    };
    let now = now_ms();
    let mut procs: Vec<Proc> = String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(|l| parse_ps_line(l, now))
        .collect();
    let cwds = cwds_of(&procs.iter().map(|p| p.pid).collect::<Vec<_>>());
    for p in &mut procs {
        p.cwd = cwds.get(&p.pid).cloned();
    }
    procs
}

/// The working directory of each pid: `/proc/<pid>/cwd`, else one `lsof` call.
fn cwds_of(pids: &[i64]) -> HashMap<i64, String> {
    let mut out = HashMap::new();
    let mut rest = Vec::new();
    for &pid in pids {
        match std::fs::read_link(format!("/proc/{pid}/cwd")) {
            Ok(p) => {
                out.insert(pid, p.display().to_string());
            }
            Err(_) => rest.push(pid.to_string()),
        }
    }
    if rest.is_empty() {
        return out;
    }
    let Ok(o) = Command::new("lsof")
        .args(["-a", "-d", "cwd", "-Fpn", "-p", &rest.join(",")])
        .output()
    else {
        return out;
    };
    let mut pid = None;
    for line in String::from_utf8_lossy(&o.stdout).lines() {
        if let Some(p) = line.strip_prefix('p') {
            pid = p.parse::<i64>().ok();
        } else if let (Some(n), Some(p)) = (line.strip_prefix('n'), pid) {
            out.insert(p, n.to_string());
        }
    }
    out
}

// --- transcripts --------------------------------------------------------------

/// A transcript's identity, from its first line and the file's mtime.
#[derive(Debug, Clone, PartialEq)]
pub struct TranscriptRef {
    pub path: PathBuf,
    pub session_id: String,
    pub cwd: String,
    pub created_ms: i64,
    pub mtime_ms: i64,
}

fn mtime_ms(p: &Path) -> Option<i64> {
    let m = std::fs::metadata(p).ok()?.modified().ok()?;
    Some(m.duration_since(UNIX_EPOCH).ok()?.as_millis() as i64)
}

fn first_line(p: &Path) -> Option<String> {
    let f = std::fs::File::open(p).ok()?;
    let mut line = String::new();
    // A Codex `session_meta` line carries the whole system prompt: read it, but cap it.
    std::io::BufReader::new(f)
        .take(4 * 1024 * 1024)
        .read_line(&mut line)
        .ok()?;
    Some(line)
}

fn iso_ms(v: Option<&Value>) -> Option<i64> {
    chrono::DateTime::parse_from_rfc3339(v?.as_str()?)
        .ok()
        .map(|d| d.timestamp_millis())
}

fn transcript_ref(h: Harness, path: &Path) -> Option<TranscriptRef> {
    let head: Value = serde_json::from_str(&first_line(path)?).ok()?;
    let meta = match h {
        Harness::Codex => {
            (head.get("type")?.as_str()? == "session_meta").then_some(head.get("payload")?)?
        }
        Harness::Pi => (head.get("type")?.as_str()? == "session").then_some(&head)?,
        Harness::Claude => return None,
    };
    let session_id = meta
        .get("id")
        .or_else(|| meta.get("session_id"))?
        .as_str()?
        .to_string();
    let cwd = meta.get("cwd")?.as_str()?.to_string();
    let mtime = mtime_ms(path)?;
    let created = iso_ms(meta.get("timestamp"))
        .or_else(|| iso_ms(head.get("timestamp")))
        .unwrap_or(mtime);
    Some(TranscriptRef {
        path: path.to_path_buf(),
        session_id,
        cwd,
        created_ms: created,
        mtime_ms: mtime,
    })
}

/// pi's per-cwd session directory name: `--Users-me-Code-app--`.
pub fn pi_dir_name(cwd: &str) -> String {
    let trimmed = cwd.trim_start_matches(['/', '\\']);
    let enc: String = trimmed
        .chars()
        .map(|c| {
            if matches!(c, '/' | '\\' | ':') {
                '-'
            } else {
                c
            }
        })
        .collect();
    format!("--{enc}--")
}

fn jsonl_in(dir: &Path) -> impl Iterator<Item = PathBuf> {
    std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "jsonl"))
}

/// Transcripts that may belong to `procs` (all of one harness): pi's per-cwd
/// directories; Codex's day directories, skipping files nobody wrote to since the
/// oldest process started (unless an id on a command line names them).
pub fn transcripts(h: Harness, root: &Path, procs: &[&Proc]) -> Vec<TranscriptRef> {
    let mut out = Vec::new();
    match h {
        Harness::Pi => {
            let cwds: HashSet<&str> = procs.iter().filter_map(|p| p.cwd.as_deref()).collect();
            for cwd in cwds {
                let dir = root.join("sessions").join(pi_dir_name(cwd));
                out.extend(jsonl_in(&dir).filter_map(|p| transcript_ref(h, &p)));
            }
        }
        Harness::Codex => {
            let since = procs.iter().map(|p| p.start_ms).min().unwrap_or(0) - 60_000;
            let wanted: Vec<String> = procs.iter().filter_map(|p| p.explicit_session()).collect();
            // Files live under the day they were created, and a long session keeps
            // writing to an old day's file — so every day is walked, by mtime.
            let sessions = root.join("sessions");
            for y in subdirs(&sessions) {
                for m in subdirs(&y) {
                    for d in subdirs(&m) {
                        for f in jsonl_in(&d) {
                            let name = f.file_name().and_then(|n| n.to_str()).unwrap_or("");
                            let named = wanted.iter().any(|w| name.contains(w.as_str()));
                            let recent = mtime_ms(&f).is_some_and(|m| m >= since);
                            if (named || recent)
                                && let Some(t) = transcript_ref(h, &f)
                            {
                                out.push(t);
                            }
                        }
                    }
                }
            }
        }
        Harness::Claude => {}
    }
    out
}

fn subdirs(p: &Path) -> Vec<PathBuf> {
    std::fs::read_dir(p)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.is_dir())
        .collect()
}

/// Pair each process with its transcript (see the module docs). Later-started
/// processes choose first, so two sessions in one cwd each get their own.
pub fn assign(procs: &[&Proc], ts: &[TranscriptRef]) -> HashMap<i64, TranscriptRef> {
    let mut out = HashMap::new();
    let mut taken: HashSet<&Path> = HashSet::new();
    for p in procs {
        if let Some(id) = p.explicit_session()
            && let Some(t) = ts
                .iter()
                .filter(|t| t.session_id == id || t.session_id.starts_with(&id))
                .max_by_key(|t| t.mtime_ms)
        {
            taken.insert(&t.path);
            out.insert(p.pid, t.clone());
        }
    }
    let mut order: Vec<&&Proc> = procs.iter().filter(|p| !out.contains_key(&p.pid)).collect();
    order.sort_by_key(|p| std::cmp::Reverse(p.start_ms));
    // Created after start (with a little clock slack), then merely written since.
    for pass in 0..2 {
        for p in &order {
            if out.contains_key(&p.pid) {
                continue;
            }
            let Some(cwd) = p.cwd.as_deref() else {
                continue;
            };
            let pick = ts
                .iter()
                .filter(|t| t.cwd == cwd && !taken.contains(t.path.as_path()))
                .filter(|t| {
                    if pass == 0 {
                        t.created_ms >= p.start_ms - 3000
                    } else {
                        t.mtime_ms >= p.start_ms
                    }
                })
                .max_by_key(|t| t.mtime_ms);
            if let Some(t) = pick {
                taken.insert(&t.path);
                out.insert(p.pid, t.clone());
            }
        }
    }
    out
}

/// What a transcript's tail says about its session.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct State {
    /// `busy` | `idle` | `waiting`.
    pub status: String,
    /// The session's display name (pi's `/name`).
    pub name: Option<String>,
    pub waiting_for: Option<String>,
    pub model: Option<String>,
    pub context: Option<ContextUsage>,
}

/// The last `max` bytes of a file, starting at a line boundary.
fn tail(path: &Path, max: u64) -> Option<String> {
    let mut f = std::fs::File::open(path).ok()?;
    let len = f.metadata().ok()?.len();
    let start = len.saturating_sub(max);
    f.seek(SeekFrom::Start(start)).ok()?;
    let mut buf = Vec::new();
    f.read_to_end(&mut buf).ok()?;
    let text = String::from_utf8_lossy(&buf).into_owned();
    Some(if start > 0 {
        text.split_once('\n')
            .map(|(_, r)| r.to_string())
            .unwrap_or_default()
    } else {
        text
    })
}

pub fn state(h: Harness, path: &Path) -> State {
    let Some(text) = tail(path, 512 * 1024) else {
        return State {
            status: "idle".into(),
            ..Default::default()
        };
    };
    match h {
        Harness::Codex => codex_state(&text),
        Harness::Pi => pi_state(&text),
        Harness::Claude => State::default(),
    }
}

fn codex_state(text: &str) -> State {
    let mut st = State {
        status: "idle".into(),
        ..Default::default()
    };
    let mut window = None;
    let mut used = None;
    for line in text.lines() {
        let Ok(v) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        let p = v.get("payload");
        let kind = v.get("type").and_then(Value::as_str).unwrap_or("");
        let ptype = p
            .and_then(|p| p.get("type"))
            .and_then(Value::as_str)
            .unwrap_or("");
        match (kind, ptype) {
            ("turn_context", _) => {
                if let Some(m) = p.and_then(|p| p.get("model")).and_then(Value::as_str) {
                    st.model = Some(m.to_string());
                }
            }
            ("event_msg", "task_started") => {
                st.status = "busy".into();
                st.waiting_for = None;
            }
            ("event_msg", "task_complete" | "turn_aborted") => {
                st.status = "idle".into();
                st.waiting_for = None;
            }
            ("event_msg", t) if t.ends_with("approval_request") || t == "request_user_input" => {
                st.status = "waiting".into();
                st.waiting_for = Some(t.trim_end_matches("_request").replace('_', " "));
            }
            ("event_msg", "token_count") => {
                let info = p.and_then(|p| p.get("info"));
                if let Some(w) = info
                    .and_then(|i| i.get("model_context_window"))
                    .and_then(Value::as_u64)
                {
                    window = Some(w);
                }
                if let Some(u) = info
                    .and_then(|i| i.get("last_token_usage"))
                    .and_then(|u| u.get("input_tokens"))
                    .and_then(Value::as_u64)
                {
                    used = Some(u);
                }
            }
            // Anything the agent does after an approval means it was answered.
            ("event_msg" | "response_item", _)
                if st.status == "waiting" && !matches!(ptype, "token_count" | "") =>
            {
                st.status = "busy".into();
                st.waiting_for = None;
            }
            _ => {}
        }
    }
    if let (Some(u), Some(w)) = (used, window) {
        st.context = Some(ContextUsage::new(u, w, st.model.clone()));
    }
    st
}

fn pi_state(text: &str) -> State {
    let mut st = State {
        status: "idle".into(),
        ..Default::default()
    };
    for line in text.lines() {
        let Ok(v) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        match v.get("type").and_then(Value::as_str) {
            Some("session_info") => {
                st.name = v
                    .get("name")
                    .and_then(Value::as_str)
                    .map(str::trim)
                    .filter(|n| !n.is_empty())
                    .map(str::to_string);
            }
            Some("model_change") => {
                st.model = v.get("modelId").and_then(Value::as_str).map(str::to_string);
            }
            Some("message") => {
                let Some(m) = v.get("message") else { continue };
                let role = m.get("role").and_then(Value::as_str).unwrap_or("");
                st.status = match role {
                    "user" | "toolResult" | "bashExecution" => "busy",
                    "assistant" => {
                        if let Some(model) = m.get("model").and_then(Value::as_str) {
                            st.model = Some(model.to_string());
                        }
                        if let Some(u) = m.get("usage") {
                            let n = |k: &str| u.get(k).and_then(Value::as_u64).unwrap_or(0);
                            let used = n("input") + n("cacheRead") + n("cacheWrite");
                            if used > 0 {
                                st.context = Some(ContextUsage::new(used, 0, st.model.clone()));
                            }
                        }
                        match m.get("stopReason").and_then(Value::as_str) {
                            Some("toolUse") => "busy",
                            _ => "idle",
                        }
                    }
                    _ => st.status.as_str(),
                }
                .to_string();
            }
            _ => {}
        }
    }
    // pi's transcript has no context window; leave usage out rather than guess one.
    st.context = None;
    st
}

/// Codex's thread names (`/rename`, or the title it generates after the first turn):
/// the last entry per id in `$CODEX_HOME/session_index.jsonl`.
pub fn codex_names(home: &Path) -> HashMap<String, String> {
    let mut out = HashMap::new();
    let Some(text) = tail(&home.join("session_index.jsonl"), 2 * 1024 * 1024) else {
        return out;
    };
    for line in text.lines() {
        let Ok(v) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        if let (Some(id), Some(name)) = (
            v.get("id").and_then(Value::as_str),
            v.get("thread_name").and_then(Value::as_str),
        ) && !name.trim().is_empty()
        {
            out.insert(id.to_string(), name.trim().to_string());
        }
    }
    out
}

/// The first genuine user prompt (Codex wraps its own context in user messages too).
pub fn first_prompt(h: Harness, path: &Path) -> Option<String> {
    prompts(h, path, 1).into_iter().next()
}

/// The first `n` genuine user prompts, in order.
pub fn prompts(h: Harness, path: &Path, n: usize) -> Vec<String> {
    let mut out = Vec::new();
    let Ok(f) = std::fs::File::open(path) else {
        return out;
    };
    for line in std::io::BufReader::new(f).take(16 * 1024 * 1024).lines() {
        let Ok(line) = line else { break };
        if !line.contains("\"user\"") {
            continue;
        }
        let Ok(v) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        let msg = match h {
            Harness::Codex => {
                let Some(p) = v.get("payload") else {
                    continue;
                };
                if v.get("type").and_then(Value::as_str) != Some("response_item")
                    || p.get("type").and_then(Value::as_str) != Some("message")
                    || p.get("role").and_then(Value::as_str) != Some("user")
                {
                    continue;
                }
                p
            }
            Harness::Pi => {
                let Some(m) = v.get("message") else {
                    continue;
                };
                if m.get("role").and_then(Value::as_str) != Some("user") {
                    continue;
                }
                m
            }
            Harness::Claude => return out,
        };
        let text = content_text(msg.get("content"));
        let t = text.trim();
        if t.is_empty() || is_codex_context(t) {
            continue;
        }
        out.push(t.to_string());
        if out.len() >= n {
            break;
        }
    }
    out
}

fn is_codex_context(t: &str) -> bool {
    t.starts_with('<') || t.starts_with("# AGENTS.md")
}

/// The text parts of a message `content` (a string or a list of parts).
pub fn content_text(c: Option<&Value>) -> String {
    match c {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Array(parts)) => parts
            .iter()
            .filter_map(|p| p.get("text").and_then(Value::as_str))
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn argv(s: &str) -> Vec<String> {
        s.split_whitespace().map(str::to_string).collect()
    }

    #[test]
    fn interactive_processes_are_told_from_the_rest() {
        assert_eq!(classify(&argv("codex")), Some(Harness::Codex));
        assert_eq!(
            classify(&argv("/opt/homebrew/bin/codex -m gpt-6 hi")),
            Some(Harness::Codex)
        );
        assert_eq!(classify(&argv("codex resume abc")), Some(Harness::Codex));
        assert_eq!(classify(&argv("codex exec hi")), None);
        assert_eq!(classify(&argv("codex app-server --listen unix://")), None);
        assert_eq!(classify(&argv("pi")), Some(Harness::Pi));
        assert_eq!(
            classify(&argv("pi --model openai/gpt-5.5 fix it")),
            Some(Harness::Pi)
        );
        assert_eq!(classify(&argv("pi -p hi")), None);
        assert_eq!(classify(&argv("pi --mode rpc")), None);
        assert_eq!(classify(&argv("pi install x")), None);
        assert_eq!(classify(&argv("claude")), None);
        assert_eq!(classify(&argv("spindump")), None);
    }

    #[test]
    fn ps_lines_become_processes() {
        let p = parse_ps_line("  4242 01:02:03 ttys004 codex -m gpt-6", 10_000_000).unwrap();
        assert_eq!(p.pid, 4242);
        assert_eq!(p.tty, "ttys004");
        assert_eq!(p.start_ms, 10_000_000 - 3723 * 1000);
        assert_eq!(p.model_arg().as_deref(), Some("gpt-6"));
        assert!(parse_ps_line("1 00:01 ?? codex", 0).is_none());
        assert_eq!(parse_etime("2-00:00:01"), Some(172_801));
        assert_eq!(parse_etime("05:07"), Some(307));
    }

    #[test]
    fn explicit_ids_come_from_resume_flags() {
        assert_eq!(
            explicit_session(Harness::Codex, &argv("codex resume abc-1")).as_deref(),
            Some("abc-1")
        );
        assert_eq!(
            explicit_session(Harness::Codex, &argv("codex fork abc")),
            None
        );
        assert_eq!(
            explicit_session(Harness::Pi, &argv("pi --session-id x9")).as_deref(),
            Some("x9")
        );
    }

    #[test]
    fn pi_session_dirs_encode_the_cwd() {
        assert_eq!(pi_dir_name("/Users/me/Code/app"), "--Users-me-Code-app--");
    }

    #[test]
    fn resume_commands_per_harness() {
        assert_eq!(
            Harness::Claude.resume_command("claude", None, "s1"),
            "claude --resume s1"
        );
        assert_eq!(
            Harness::Codex.resume_command("codex", Some("gpt-6"), "s1"),
            "codex --model 'gpt-6' resume s1"
        );
        assert_eq!(
            Harness::Pi.resume_command("pi", None, "s1"),
            "pi --session s1"
        );
    }

    fn proc_(pid: i64, cwd: &str, start: i64) -> Proc {
        Proc {
            pid,
            tty: "ttys001".into(),
            harness: Harness::Pi,
            start_ms: start,
            argv: argv("pi"),
            cwd: Some(cwd.into()),
        }
    }

    fn tref(id: &str, cwd: &str, created: i64, mtime: i64) -> TranscriptRef {
        TranscriptRef {
            path: PathBuf::from(format!("/t/{id}.jsonl")),
            session_id: id.into(),
            cwd: cwd.into(),
            created_ms: created,
            mtime_ms: mtime,
        }
    }

    #[test]
    fn two_sessions_in_one_cwd_get_their_own_transcripts() {
        let a = proc_(1, "/w", 1_000_000);
        let b = proc_(2, "/w", 2_000_000);
        let ts = vec![
            tref("old", "/w", 10, 900_000),
            tref("a", "/w", 1_000_500, 2_500_000),
            tref("b", "/w", 2_000_400, 2_400_000),
            tref("elsewhere", "/x", 2_000_400, 3_000_000),
        ];
        let got = assign(&[&a, &b], &ts);
        assert_eq!(got[&1].session_id, "a");
        assert_eq!(got[&2].session_id, "b");
    }

    #[test]
    fn a_resumed_session_falls_back_to_a_transcript_written_since_start() {
        let a = proc_(1, "/w", 1_000_000);
        let ts = vec![tref("old", "/w", 10, 1_500_000), tref("older", "/w", 5, 20)];
        assert_eq!(assign(&[&a], &ts)[&1].session_id, "old");
        let idle = proc_(2, "/w", 2_000_000);
        assert!(assign(&[&idle], &ts).is_empty());
    }

    #[test]
    fn codex_status_follows_the_turn_events() {
        let ev = |t: &str| format!(r#"{{"type":"event_msg","payload":{{"type":"{t}"}}}}"#);
        let busy = [ev("task_started"), ev("item_completed")].join("\n");
        assert_eq!(codex_state(&busy).status, "busy");
        let done = [ev("task_started"), ev("task_complete")].join("\n");
        assert_eq!(codex_state(&done).status, "idle");
        let wait = [ev("task_started"), ev("exec_approval_request")].join("\n");
        let st = codex_state(&wait);
        assert_eq!(st.status, "waiting");
        assert_eq!(st.waiting_for.as_deref(), Some("exec approval"));
        let tc = r#"{"type":"event_msg","payload":{"type":"token_count","info":{"last_token_usage":{"input_tokens":25840},"model_context_window":258400}}}"#;
        let ctx = r#"{"type":"turn_context","payload":{"model":"gpt-6"}}"#;
        let st = codex_state(&[ctx, tc].join("\n"));
        let c = st.context.unwrap();
        assert_eq!((c.used, c.window, c.pct), (25840, 258400, 10));
        assert_eq!(st.model.as_deref(), Some("gpt-6"));
    }

    #[test]
    fn pi_status_follows_the_last_message() {
        let m = |role: &str, stop: &str| {
            format!(
                r#"{{"type":"message","message":{{"role":"{role}","stopReason":"{stop}","model":"gpt-5.5"}}}}"#
            )
        };
        assert_eq!(pi_state(&m("user", "")).status, "busy");
        assert_eq!(
            pi_state(&[m("user", ""), m("assistant", "toolUse")].join("\n")).status,
            "busy"
        );
        let st = pi_state(&[m("user", ""), m("assistant", "stop")].join("\n"));
        assert_eq!(st.status, "idle");
        assert_eq!(st.model.as_deref(), Some("gpt-5.5"));
        let named = [
            m("user", ""),
            r#"{"type":"session_info","name":"auth-fix"}"#.to_string(),
        ];
        assert_eq!(
            pi_state(&named.join("\n")).name.as_deref(),
            Some("auth-fix")
        );
    }

    #[test]
    fn codex_names_keep_the_last_entry_per_thread() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("session_index.jsonl"),
            concat!(
                r#"{"id":"a","thread_name":"Reply pong"}"#,
                "\n",
                r#"{"id":"b","thread_name":"Other"}"#,
                "\n",
                r#"{"id":"a","thread_name":"codex-probe"}"#,
                "\n"
            ),
        )
        .unwrap();
        let n = codex_names(dir.path());
        assert_eq!(n["a"], "codex-probe");
        assert_eq!(n["b"], "Other");
    }

    #[test]
    fn transcripts_are_read_from_their_first_line() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let pdir = root.join("sessions").join(pi_dir_name("/w/app"));
        std::fs::create_dir_all(&pdir).unwrap();
        let f = pdir.join("2026-10-11T19-50-44-044Z_p1.jsonl");
        std::fs::write(
            &f,
            concat!(
                r#"{"type":"session","version":3,"id":"p1","timestamp":"2026-10-11T19:50:44.044Z","cwd":"/w/app"}"#,
                "\n",
                r#"{"type":"message","message":{"role":"user","content":[{"type":"text","text":"fix the bug"}]}}"#,
                "\n"
            ),
        )
        .unwrap();
        let p = proc_(1, "/w/app", 0);
        let ts = transcripts(Harness::Pi, root, &[&p]);
        assert_eq!(ts.len(), 1);
        assert_eq!(ts[0].session_id, "p1");
        assert_eq!(ts[0].created_ms, 1_791_748_244_044);
        assert_eq!(
            first_prompt(Harness::Pi, &f).as_deref(),
            Some("fix the bug")
        );

        let cdir = root.join("codex/sessions/2026/10/11");
        std::fs::create_dir_all(&cdir).unwrap();
        let cf = cdir.join("rollout-2026-10-11T21-50-53-c1.jsonl");
        std::fs::write(
            &cf,
            concat!(
                r#"{"timestamp":"2026-10-11T19:50:53.000Z","type":"session_meta","payload":{"id":"c1","timestamp":"2026-10-11T19:50:53.000Z","cwd":"/w/app"}}"#,
                "\n",
                r#"{"type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"<environment_context>x</environment_context>"}]}}"#,
                "\n",
                r#"{"type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"reply pong"}]}}"#,
                "\n"
            ),
        )
        .unwrap();
        let mut cp = proc_(2, "/w/app", now_ms() - 10_000);
        cp.harness = Harness::Codex;
        let ts = transcripts(Harness::Codex, &root.join("codex"), &[&cp]);
        assert_eq!(ts.len(), 1);
        assert_eq!(ts[0].session_id, "c1");
        assert_eq!(
            first_prompt(Harness::Codex, &cf).as_deref(),
            Some("reply pong")
        );
    }

    #[test]
    fn a_launcher_path_must_be_executable() {
        let dir = tempfile::tempdir().unwrap();
        let exe = dir.path().join("agent");
        std::fs::write(&exe, "#!/bin/sh\n").unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&exe, std::fs::Permissions::from_mode(0o755)).unwrap();
        let exe = exe.display().to_string();
        let elsewhere = tempfile::tempdir().unwrap();
        let check = |l: &str| check_launcher(l, elsewhere.path());
        assert_eq!(check(&exe), LauncherCheck::Found);
        assert_eq!(check(&format!("{exe} --model 'x'")), LauncherCheck::Found);
        let missing = dir.path().join("nope").display().to_string();
        assert_eq!(check(&missing), LauncherCheck::Missing);
        // A relative path is judged from the spawn's dir, not from where fleet runs.
        assert_eq!(check_launcher("./agent", dir.path()), LauncherCheck::Found);
        assert_eq!(check("./agent"), LauncherCheck::Missing);
        // Not a plain command word: no way to tell, so the spawn goes ahead.
        assert_eq!(check("FOO=1 claude"), LauncherCheck::Unknown);
        assert_eq!(check("$HOME/bin/x"), LauncherCheck::Unknown);
        assert_eq!(check(""), LauncherCheck::Unknown);
    }

    #[test]
    fn pi_models_parse_the_table() {
        let t = "provider  model     context\nopenai    gpt-5.5   272K\nanthropic claude-x  1M\n";
        let m = parse_pi_models(t);
        assert_eq!(m.len(), 2);
        assert_eq!(m[0].id, "openai/gpt-5.5");
        assert_eq!(m[1].label, "claude-x (anthropic)");
    }
}
