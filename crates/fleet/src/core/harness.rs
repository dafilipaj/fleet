//! Coding-agent harnesses: Claude Code, Codex and pi.
//!
//! Claude Code keeps a live registry (`~/.claude/sessions/<pid>.json`, see
//! [`crate::core::discovery`]); the others don't. For them this module finds the
//! interactive processes with `ps`, matches each to the transcript it writes, and
//! reads the status, first prompt, model, effort and context use from that transcript's
//! tail:
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
//! was created after it started, then the newest one written to since; a Codex child
//! thread's rollout (`spawn_agent`, `/review`) shares its parent's cwd and is skipped. A
//! fresh Codex session has no transcript until its first prompt — it is listed without a
//! session id.

use std::collections::{HashMap, HashSet};
use std::io::{BufRead, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant, UNIX_EPOCH};

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

    /// Whether its CLI ends options at `--`, so a prompt that starts with `-` (or is a
    /// subcommand's name, like Codex's `resume`) still arrives as the prompt. Claude and Codex
    /// do; pi's parser has no `--` (it would read it as a flag and swallow the prompt).
    pub fn ends_options_at_double_dash(self) -> bool {
        matches!(self, Harness::Claude | Harness::Codex)
    }

    /// The slash command that renames a live session.
    pub fn rename_command(self, title: &str) -> String {
        match self {
            Harness::Claude | Harness::Codex => format!("/rename {title}"),
            Harness::Pi => format!("/name {title}"),
        }
    }

    /// The slash command that ends a live session, when it has one (`fleet close` types it).
    /// pi's is not relied on: `close` ends a pi session with its tmux session.
    pub fn exit_command(self) -> Option<&'static str> {
        match self {
            Harness::Claude => Some("/exit"),
            Harness::Codex => Some("/quit"),
            Harness::Pi => None,
        }
    }

    /// Arguments that resume `session_id` (appended to the launcher). The id is bound so the
    /// harness can never read it as a flag: Claude's as `--resume=<id>`, Codex's after `--`
    /// (`codex resume -- <id>`: its positional session id, even one that starts with `-`).
    pub fn resume_args(self, session_id: &str) -> String {
        let id = crate::core::tools::shq_min(session_id);
        match self {
            Harness::Claude => format!("--resume={id}"),
            Harness::Codex => format!("resume -- {id}"),
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
    /// The effort levels it runs at, lowest first: what `fleet effort` (Claude Code) can be
    /// given for it. `[]` where the harness has none (pi) or fleet does not know them.
    #[serde(default)]
    pub efforts: Vec<String>,
    /// The level a new session on it starts at, when known.
    #[serde(default)]
    pub default_effort: Option<String>,
}

fn opt(id: &str, label: &str) -> ModelOption {
    ModelOption {
        id: id.into(),
        label: label.into(),
        efforts: Vec::new(),
        default_effort: None,
    }
}

/// The effort levels of every model on Claude's picker, lowest first (Claude Code's model docs
/// as of 2.1.306: a level a model lacks runs as the highest it has). Claude Code 2.1.308 ranks
/// its levels in this order too. `auto`, which clears the saved level, is no level a model
/// runs at.
const CLAUDE_EFFORTS: [&str; 5] = ["low", "medium", "high", "xhigh", "max"];

/// The levels Claude Code 2.1.308 saves in its settings (`effortLevel`, top-level or per
/// model); a value outside them is dropped as unset when it reads them.
const CLAUDE_SAVED_EFFORTS: [&str; 4] = ["low", "medium", "high", "xhigh"];

/// Codex's reasoning levels, lowest first: its reasoning-effort enum, in declared order.
const CODEX_EFFORTS: [&str; 8] = [
    "none", "minimal", "low", "medium", "high", "xhigh", "max", "ultra",
];

/// Claude's listed models: id, label, and the level a new session on it starts at by Claude
/// Code's docs.
const CLAUDE_LISTED: [(&str, &str, &str); 4] = [
    ("claude-fable-5-1", "Fable 5.1", "high"),
    ("claude-opus-5-5", "Opus 5.5", "medium"),
    ("claude-sonnet-5-5", "Sonnet 5.5", "medium"),
    ("claude-haiku-5-5", "Haiku 5.5", "medium"),
];

/// `levels`, lowest first by `order`, each once; a level `order` does not name is dropped.
fn ranked<'a>(levels: impl IntoIterator<Item = &'a String>, order: &[&str]) -> Vec<String> {
    let levels: HashSet<&str> = levels.into_iter().map(String::as_str).collect();
    order
        .iter()
        .filter(|o| levels.contains(*o))
        .map(|o| o.to_string())
        .collect()
}

/// The rank of `level` in `order` (`None` if it is not there).
fn rank(level: &str, order: &[&str]) -> Option<usize> {
    order.iter().position(|o| *o == level)
}

/// Claude's picker (the web's `web.models` overrides it there), its default labelled
/// with the model Claude Code would start with — see [`claude_default_model`]. Each model
/// with its effort levels and the level it starts at by Claude Code's docs, hand-kept like the
/// list itself; [`claude_models_saved`] applies what Claude Code's settings say.
pub fn claude_models(default: Option<&str>) -> Vec<ModelOption> {
    claude_models_saved(default, &SavedEfforts::default())
}

/// [`claude_models`], each model's `default_effort` the level `saved` says a new session on it
/// starts at ([`SavedEfforts::starts`]), `null` where that is not certain. The default entry
/// takes the levels of the model `default` names (an alias resolved through this list), else
/// every level any model has, and no default level.
pub fn claude_models_saved(default: Option<&str>, saved: &SavedEfforts) -> Vec<ModelOption> {
    let listed: Vec<ModelOption> = CLAUDE_LISTED
        .iter()
        .map(|(id, label, starts)| ModelOption {
            efforts: CLAUDE_EFFORTS.iter().map(|e| e.to_string()).collect(),
            default_effort: saved.starts(id, starts),
            ..opt(id, label)
        })
        .collect();
    let named = default
        .filter(|d| !d.trim().is_empty())
        .and_then(crate::core::model::wanted)
        .and_then(|label| listed.iter().find(|m| m.label == label));
    let mut out = vec![default_entry(default, named, &listed, &CLAUDE_EFFORTS)];
    out.extend(listed);
    out
}

/// The `id: ""` entry, labelled after `default` ([`default_label`]): the levels of `named`, the
/// listed model it names, else every level of any `listed` model (lowest first by `order`,
/// one `order` does not name dropped) and no default level.
fn default_entry(
    default: Option<&str>,
    named: Option<&ModelOption>,
    listed: &[ModelOption],
    order: &[&str],
) -> ModelOption {
    let (efforts, default_effort) = match named {
        Some(m) => (m.efforts.clone(), m.default_effort.clone()),
        None => (ranked(listed.iter().flat_map(|m| &m.efforts), order), None),
    };
    ModelOption {
        efforts,
        default_effort,
        ..opt("", &default_label(default))
    }
}

/// What decides the level a new Claude Code session on each listed model starts at, as far as
/// fleet reads it ([`claude_saved_efforts`]): the user `settings.json` and fleet's own
/// environment. Claude Code 2.1.308 also reads project, local and managed settings, an
/// organization's caps and its own state; none of those is read here.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SavedEfforts {
    /// `modelSettings.<key>.effortLevel` by the listed model id `<key>` names: the entry keyed
    /// by that very id, else the first naming it otherwise (an alias, `[1m]`, a date), as
    /// Claude Code takes it.
    levels: HashMap<String, String>,
    /// A top-level `effortLevel` is saved. Claude Code applies it to some models and not to
    /// others, by lists of its own: where it might apply, the level is not certain.
    top_level: bool,
    /// `modelSettings.<key>.maxEffortLevel` by the listed model id `<key>` names, the lowest
    /// of those naming it.
    model_caps: HashMap<String, String>,
    /// The top-level `maxEffortLevel`.
    cap: Option<String>,
    /// `CLAUDE_CODE_EFFORT_LEVEL` is set: it overrides every saved level.
    env: bool,
}

impl SavedEfforts {
    /// The level a new session on listed model `id` starts at, its table level `table` aside:
    /// its saved level, else `table` — `None` when `CLAUDE_CODE_EFFORT_LEVEL` is set, or when
    /// nothing is saved for it but a top-level `effortLevel` is. Then held to its cap: its own
    /// `maxEffortLevel`, else the top-level one; `max` is no cap.
    pub fn starts(&self, id: &str, table: &str) -> Option<String> {
        if self.env {
            return None;
        }
        let level = match self.levels.get(id) {
            Some(l) => l.as_str(),
            None if self.top_level => return None,
            None => table,
        };
        let cap = self.model_caps.get(id).or(self.cap.as_ref());
        let capped = match cap {
            Some(c) if c != "max" && rank(level, &CLAUDE_EFFORTS) > rank(c, &CLAUDE_EFFORTS) => {
                c.as_str()
            }
            _ => level,
        };
        Some(capped.to_string())
    }
}

/// Claude Code's user config dir: `$CLAUDE_CONFIG_DIR`, else `~/.claude`.
pub fn claude_config_dir() -> PathBuf {
    std::env::var_os("CLAUDE_CONFIG_DIR")
        .filter(|p| !p.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| dirs::home_dir().unwrap_or_default().join(".claude"))
}

/// Settings files larger than this are not read: the default is then unknown.
const MAX_SETTINGS_BYTES: u64 = 1 << 20;

/// The model a new Claude Code session starts with, as Claude Code resolves it at user
/// level: `env_model` (`ANTHROPIC_MODEL`), else `model` in `<config_dir>/settings.json`,
/// else unknown. The value is kept as written (`opus[1m]`, an alias, a full id).
///
/// Project settings (`.claude/settings.json` and `settings.local.json` under the start
/// directory) can override it too, but they depend on where a session is started, so this
/// harness-wide default leaves them out.
///
/// Only the `model` key is taken from the settings: the file can hold secrets (`env`,
/// `apiKeyHelper`), so nothing else in it is kept or logged. A missing, oversized or
/// invalid file means unknown.
pub fn claude_default_model(env_model: Option<&str>, config_dir: &Path) -> Option<String> {
    if let Some(m) = env_model.map(str::trim).filter(|m| !m.is_empty()) {
        return Some(m.to_string());
    }
    claude_settings(config_dir)?
        .get("model")?
        .as_str()
        .map(str::trim)
        .filter(|m| !m.is_empty())
        .map(str::to_string)
}

/// What Claude Code's user settings and `env_level` (`CLAUDE_CODE_EFFORT_LEVEL`) say about the
/// level each listed model starts at ([`SavedEfforts`]), read as Claude Code 2.1.308 reads
/// them: from `<config_dir>/settings.json`, `effortLevel` (`low` to `xhigh`, what `/effort`
/// saves; `auto` removes it) and `maxEffortLevel` (`low` to `max`), top-level and in each
/// `modelSettings.<model>`. A key there counts for the listed model it names, as an id, an
/// alias or with a `[1m]` or date suffix; one fleet cannot resolve (another provider's
/// spelling) is skipped. A value of another word is unset, as Claude Code drops it. Only these
/// values are taken from the file, as for [`claude_default_model`]; a missing, oversized or
/// invalid file is none saved.
pub fn claude_saved_efforts(config_dir: &Path, env_level: Option<&str>) -> SavedEfforts {
    let mut saved = SavedEfforts {
        env: env_level.is_some_and(|l| !l.trim().is_empty()),
        ..SavedEfforts::default()
    };
    let Some(settings) = claude_settings(config_dir) else {
        return saved;
    };
    let level = |v: Option<&Value>, of: &[&str]| {
        v.and_then(Value::as_str)
            .filter(|l| of.contains(l))
            .map(str::to_string)
    };
    saved.top_level = level(settings.get("effortLevel"), &CLAUDE_SAVED_EFFORTS).is_some();
    saved.cap = level(settings.get("maxEffortLevel"), &CLAUDE_EFFORTS);
    let listed = |key: &str| {
        let label = crate::core::model::wanted(key)?;
        CLAUDE_LISTED
            .iter()
            .find(|(_, l, _)| *l == label)
            .map(|(id, _, _)| id.to_string())
    };
    let entries = settings.get("modelSettings").and_then(Value::as_object);
    for (key, m) in entries.into_iter().flatten() {
        let Some(id) = listed(key) else {
            continue;
        };
        if let Some(l) = level(m.get("effortLevel"), &CLAUDE_SAVED_EFFORTS)
            && (*key == id || !saved.levels.contains_key(&id))
        {
            saved.levels.insert(id.clone(), l);
        }
        if let Some(c) = level(m.get("maxEffortLevel"), &CLAUDE_EFFORTS) {
            let lower = saved
                .model_caps
                .get(&id)
                .is_none_or(|was| rank(&c, &CLAUDE_EFFORTS) < rank(was, &CLAUDE_EFFORTS));
            if lower {
                saved.model_caps.insert(id, c);
            }
        }
    }
    saved
}

/// `<config_dir>/settings.json`, parsed; `None` when missing, larger than
/// [`MAX_SETTINGS_BYTES`] or invalid. Callers take only the keys they name from it: it can
/// hold secrets.
fn claude_settings(config_dir: &Path) -> Option<Value> {
    let file = std::fs::File::open(config_dir.join("settings.json")).ok()?;
    let mut text = String::new();
    file.take(MAX_SETTINGS_BYTES + 1)
        .read_to_string(&mut text)
        .ok()?;
    if text.len() as u64 > MAX_SETTINGS_BYTES {
        return None;
    }
    serde_json::from_str(&text).ok()
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
/// priority order), after the default — labelled with `model` from `config.toml`. Each with its
/// reasoning levels from the same cache (`supported_reasoning_levels`' `effort`s, lowest first
/// by Codex's own order, [`CODEX_EFFORTS`], a level it does not name dropped;
/// `default_reasoning_level`, if it names one); the default entry those of the model it names,
/// else every level and none for its default.
pub fn codex_models(home: &Path) -> Vec<ModelOption> {
    let default = std::fs::read_to_string(home.join("config.toml"))
        .ok()
        .and_then(|t| {
            t.lines().find_map(|l| {
                let (k, v) = l.split_once('=')?;
                (k.trim() == "model").then(|| v.trim().trim_matches('"').to_string())
            })
        });
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
            let listed: Vec<String> = m
                .get("supported_reasoning_levels")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(|l| Some(l.get("effort")?.as_str()?.to_string()))
                .collect();
            let efforts = ranked(&listed, &CODEX_EFFORTS);
            let default_effort = m
                .get("default_reasoning_level")
                .and_then(Value::as_str)
                .filter(|l| CODEX_EFFORTS.contains(l))
                .map(str::to_string);
            Some((
                prio,
                ModelOption {
                    efforts,
                    default_effort,
                    ..opt(id, label)
                },
            ))
        })
        .collect();
    models.sort_by_key(|(p, _)| *p);
    let listed: Vec<ModelOption> = models.into_iter().map(|(_, m)| m).collect();
    let named = default
        .as_deref()
        .and_then(|d| listed.iter().find(|m| m.id == d));
    let mut out = vec![default_entry(
        default.as_deref(),
        named,
        &listed,
        &CODEX_EFFORTS,
    )];
    out.extend(listed);
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

/// How much of a probe's stdout is kept (the rest is read and dropped, so the child never blocks).
const PROBE_OUTPUT_CAP: usize = 64 * 1024;

/// What a probe printed (up to [`PROBE_OUTPUT_CAP`] bytes, invalid UTF-8 replaced) and whether it
/// finished in time: it saw its end marker, or exited successfully with its stdout closed.
struct Captured {
    out: String,
    done: bool,
}

/// Whether `text` has a complete line that is exactly `marker`.
fn has_marker_line(text: &str, marker: &str) -> bool {
    let mut lines = text.split('\n');
    lines.next_back(); // the unfinished last line
    lines.any(|l| l.trim_end_matches('\r') == marker)
}

/// Run `cmd` (no input, stderr dropped) in its own session (its own process group, and no
/// controlling terminal: an interactive zsh in a background group would stop on SIGTTOU) until
/// `deadline`, collecting its stdout. It ends the moment a line `marker` shows up, when given: a
/// profile's background job that keeps stdout open does not delay the answer. A child still
/// running, or one that left a job holding stdout open, is killed with its whole group (the shell
/// is reaped only after, so its pgid is still ours), so a `sleep` does not outlive it. At the deadline what was
/// printed so far is returned, not `done`. `None` only when the command can't start.
fn capture(cmd: &mut Command, deadline: Instant, marker: Option<&str>) -> Option<Captured> {
    use std::os::unix::process::CommandExt;
    use std::sync::{Arc, Mutex, mpsc};
    let mut child = unsafe {
        cmd.stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .pre_exec(|| {
                if libc::setsid() == -1 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            })
            .spawn()
            .ok()?
    };
    let pgid = child.id() as libc::pid_t;
    let mut stdout = child.stdout.take()?;
    let buf = Arc::new(Mutex::new(Vec::<u8>::new()));
    let (tx, rx) = mpsc::channel::<()>();
    let sink = Arc::clone(&buf);
    std::thread::spawn(move || {
        let mut chunk = [0u8; 4096];
        while let Ok(n) = stdout.read(&mut chunk) {
            if n == 0 {
                break;
            }
            {
                let mut b = sink.lock().unwrap_or_else(|e| e.into_inner());
                let room = PROBE_OUTPUT_CAP.saturating_sub(b.len());
                b.extend_from_slice(&chunk[..n.min(room)]);
            }
            let _ = tx.send(());
        }
        // `tx` drops here: the receiver sees the end of the stream.
    });
    let text =
        || String::from_utf8_lossy(&buf.lock().unwrap_or_else(|e| e.into_inner())).into_owned();
    let (mut exited, mut eof, mut marked) = (false, false, false);
    loop {
        if let Some(m) = marker
            && has_marker_line(&text(), m)
        {
            marked = true;
            break;
        }
        if !exited {
            exited = has_exited(pgid);
        }
        if exited && eof {
            break;
        }
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            break;
        }
        let tick = left.min(Duration::from_millis(10));
        if eof {
            std::thread::sleep(tick);
        } else if let Err(mpsc::RecvTimeoutError::Disconnected) = rx.recv_timeout(tick) {
            eof = true;
        }
    }
    if !(exited && eof) {
        // Not a clean end: the whole group goes, including a job that kept stdout open after
        // the shell exited. The shell is still unreaped (a zombie keeps its pid and pgid ours),
        // so the pgid cannot belong to anyone else yet. ESRCH (nothing left) is fine.
        unsafe { libc::killpg(pgid, libc::SIGKILL) };
        // The shell may have been mid-`fork` when the signal went out, and a child born then
        // can be missed. Once the shell is dead it forks no more: a second pass takes whatever
        // that left (still before the reap, so the pgid is still ours).
        let gone = Instant::now() + Duration::from_secs(1);
        while !has_exited(pgid) && Instant::now() < gone {
            std::thread::sleep(Duration::from_millis(1));
        }
        unsafe { libc::killpg(pgid, libc::SIGKILL) };
    }
    let status = child.wait().ok();
    Some(Captured {
        out: text(),
        done: marked || status.is_some_and(|s| s.success()) && eof,
    })
}

/// Whether the child `pid` has exited, without reaping it (`WNOWAIT`): it stays a zombie, so
/// its pid, and the process group of that id, cannot be reused until `Child::wait`.
fn has_exited(pid: libc::pid_t) -> bool {
    let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
    let rc = unsafe {
        libc::waitid(
            libc::P_PID,
            pid as libc::id_t,
            &mut info,
            libc::WEXITED | libc::WNOHANG | libc::WNOWAIT,
        )
    };
    // With WNOHANG, 0 and an untouched `info` (si_pid 0) mean "still running".
    #[cfg(target_vendor = "apple")]
    let reported = info.si_pid;
    #[cfg(not(target_vendor = "apple"))]
    let reported = unsafe { info.si_pid() };
    rc == 0 && reported == pid
}

/// stdout of `cmd`, or `None` when it fails or outlives `timeout` (then it is killed, with
/// whatever it started). A child it leaves behind holding stdout open is waited for only until
/// the same deadline.
fn run_with_timeout(cmd: &mut Command, timeout: Duration) -> Option<String> {
    let got = capture(cmd, Instant::now() + timeout, None)?;
    got.done.then_some(got.out)
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
    /// The absolute path of the executable its launcher runs ([`launcher_paths`]: the login
    /// shell's, a function or alias of that name skipped), or `null`.
    pub path: Option<String>,
    /// The first line of `<path> --version` ([`version_line`]), or `null`: not found, failed,
    /// or no answer before the lookup's deadline ([`LOOKUP_WITHIN`]).
    pub version: Option<String>,
    pub models: Vec<ModelOption>,
}

/// The whole executable lookup (login shell, then every `--version`) shares this one deadline;
/// what is not answered by then is `null`.
const LOOKUP_WITHIN: Duration = Duration::from_secs(3);

/// Every harness's row. The executables are looked up together (one login shell for all of
/// them), then each one's `--version` runs in its own thread, all while the models are read.
/// The lookup and the versions together get [`LOOKUP_WITHIN`]: the list takes no longer than
/// that, or its slowest model read.
pub fn infos(with_models: bool) -> Vec<HarnessInfo> {
    let commands: Vec<String> = Harness::ALL.iter().map(|h| h.command()).collect();
    let here = std::env::current_dir()
        .ok()
        .or_else(dirs::home_dir)
        .unwrap_or_else(|| PathBuf::from("/"));
    let deadline = Instant::now() + LOOKUP_WITHIN;
    std::thread::scope(|s| {
        let models: Vec<_> = Harness::ALL
            .iter()
            .zip(&commands)
            .map(|(h, command)| s.spawn(move || installed_and_models(*h, command, with_models)))
            .collect();
        let paths = launcher_paths(&commands, &here, deadline);
        let versions: Vec<_> = paths
            .iter()
            .map(|p| {
                let p = p.clone();
                s.spawn(move || p.as_deref().and_then(|p| version_line(p, deadline)))
            })
            .collect();
        Harness::ALL
            .iter()
            .zip(commands.iter().cloned())
            .zip(paths)
            .zip(models.into_iter().zip(versions))
            .map(|(((h, command), path), (models, version))| {
                let (installed, models) = joined(models);
                HarnessInfo {
                    id: h.id(),
                    label: h.label(),
                    command,
                    installed,
                    path: path.map(|p| p.display().to_string()),
                    version: joined(version),
                    models,
                }
            })
            .collect()
    })
}

fn joined<T>(h: std::thread::ScopedJoinHandle<'_, T>) -> T {
    h.join().unwrap_or_else(|e| std::panic::resume_unwind(e))
}

/// Whether the launcher's first word resolves to an executable (PATH, the usual install dirs),
/// and, when it does and `with_models`, the models for the picker.
fn installed_and_models(h: Harness, command: &str, with_models: bool) -> (bool, Vec<ModelOption>) {
    let bin = command.split_whitespace().next().unwrap_or(h.id());
    let installed = if bin.contains('/') {
        crate::core::tools::is_executable(Path::new(&crate::core::tools::expand_tilde(bin)))
    } else {
        crate::core::tools::find_binary(bin).is_some()
    };
    let models = match (with_models && installed, h) {
        (false, _) => Vec::new(),
        (true, Harness::Claude) => {
            let env_model = std::env::var("ANTHROPIC_MODEL").ok();
            let env_level = std::env::var("CLAUDE_CODE_EFFORT_LEVEL").ok();
            let dir = claude_config_dir();
            let default = claude_default_model(env_model.as_deref(), &dir);
            let saved = claude_saved_efforts(&dir, env_level.as_deref());
            claude_models_saved(default.as_deref(), &saved)
        }
        (true, Harness::Codex) => codex_models(&codex_home()),
        (true, Harness::Pi) => pi_models(&pi_home(), command),
    };
    (installed, models)
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
/// Under `FLEET_PATH_ONLY` `PATH` alone decides: no install dirs, no login shell.
/// Everything is judged from `dir`, where the spawned shell will run the launcher: a relative
/// path (`./agent`, `bin/agent`) is resolved there, and the login shell starts there.
pub fn check_launcher(launcher: &str, dir: &Path) -> LauncherCheck {
    let Some(word) = launcher_word(launcher) else {
        return LauncherCheck::Unknown;
    };
    if word.contains('/') {
        return if crate::core::tools::is_executable(&launcher_file(word, dir)) {
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
    // A login shell would look past PATH (its profile adds to it).
    if crate::core::tools::path_only() {
        return LauncherCheck::Missing;
    }
    login_shell_has(word, dir)
}

/// The launcher's first word, when it is one fleet can look up: a name or a path, not a flag,
/// an assignment (`VAR=… cmd`) or anything a shell would expand.
fn launcher_word(launcher: &str) -> Option<&str> {
    let word = launcher.split_whitespace().next()?;
    let plain = word
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || "._/~+-".contains(c));
    (plain && !word.starts_with('-')).then_some(word)
}

/// A launcher word with a `/`: `~` expanded, a relative one taken from `dir` (where the shell
/// will be, not where fleet runs).
fn launcher_file(word: &str, dir: &Path) -> PathBuf {
    let p = PathBuf::from(crate::core::tools::expand_tilde(word));
    if p.is_absolute() { p } else { dir.join(p) }
}

/// The executable file each launcher's first word runs, looked for where [`check_launcher`]
/// looks, but asking for the file: a path as it is (when executable); a bare name first in a
/// login shell of `$SHELL`, where a spawn types it, as its `command -v` with any function or
/// alias of that name set aside (a wrapper function runs the file of that name the same way),
/// then on `PATH` and the usual install dirs. Under `FLEET_PATH_ONLY`, `PATH` alone. One
/// login shell answers for all the names. `None` for one found nowhere, or a launcher that is
/// not a word fleet looks up (`VAR=… cmd`), and for a relative one (`./agent`): it means
/// something only where a spawn runs it, not here. Paths come back normalized (no `.`
/// components). The login shell is given until `deadline`.
pub fn launcher_paths(launchers: &[String], dir: &Path, deadline: Instant) -> Vec<Option<PathBuf>> {
    use crate::core::tools;
    let words: Vec<Option<&str>> = launchers.iter().map(|l| launcher_word(l)).collect();
    let mut names: Vec<&str> = words.iter().flatten().copied().collect();
    names.retain(|w| !w.contains('/'));
    names.sort_unstable();
    names.dedup();
    let in_shell = if names.is_empty() || tools::path_only() {
        HashMap::new()
    } else {
        login_shell_paths(&names, dir, deadline)
    };
    words
        .into_iter()
        .map(|word| {
            let word = word?;
            if word.contains('/') {
                let p = launcher_file(word, dir);
                // Not absolute once `~` is resolved: where it points depends on the spawn's dir.
                if !PathBuf::from(tools::expand_tilde(word)).is_absolute() {
                    return None;
                }
                return Some(p).filter(|p| tools::is_executable(p));
            }
            in_shell
                .get(word)
                .cloned()
                .or_else(|| tools::find_binary(word))
                .or_else(|| {
                    (word == "claude")
                        .then(tools::claude)
                        .flatten()
                        .map(PathBuf::from)
                })
        })
        .map(|p| p.map(|p| p.components().collect()))
        .collect()
}

/// An interactive login shell of `$SHELL` running `script` with `words` as `$1`, `$2`, …, in
/// `dir`; `None` for a shell that is not sh-family.
fn login_shell(script: &str, words: &[&str], dir: &Path) -> Option<Command> {
    let shell = std::env::var("SHELL")
        .ok()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "/bin/sh".into());
    let base = Path::new(&shell)
        .file_name()
        .map(|b| b.to_string_lossy().into_owned())
        .unwrap_or_default();
    if !matches!(base.as_str(), "sh" | "bash" | "zsh" | "dash" | "ksh") {
        return None;
    }
    let mut cmd = Command::new(&shell);
    cmd.args(["-l", "-i", "-c", script, "fleet"])
        .args(words)
        .env("DISABLE_AUTO_UPDATE", "true")
        .current_dir(dir);
    Some(cmd)
}

/// The last line of `out` that starts with `fleet:` — a profile may print too.
fn shell_answer(out: &str) -> Option<&str> {
    out.lines().rev().find(|l| l.starts_with("fleet:"))
}

/// `command -v <word>` in an interactive login shell of `$SHELL` (sh-family shells only).
fn login_shell_has(word: &str, dir: &Path) -> LauncherCheck {
    let Some(mut cmd) = login_shell(
        r#"if command -v "$1" >/dev/null 2>&1; then echo fleet:found; else echo fleet:missing; fi"#,
        &[word],
        dir,
    ) else {
        return LauncherCheck::Unknown;
    };
    match run_with_timeout(&mut cmd, Duration::from_secs(15))
        .as_deref()
        .and_then(shell_answer)
    {
        Some("fleet:found") => LauncherCheck::Found,
        Some("fleet:missing") => LauncherCheck::Missing,
        _ => LauncherCheck::Unknown,
    }
}

/// For each name, the file `command -v` finds in one interactive login shell of `$SHELL`, with
/// a function or alias of that name removed first (in that shell only), so the answer is the
/// file on its `PATH`, never the wrapper. Names it finds no file for are left out. The shell
/// gets until `deadline`; it is not waited for once it has printed [`SHELL_END`] (a profile's
/// background job may hold its stdout open), and answers it gave before the deadline are kept.
fn login_shell_paths(names: &[&str], dir: &Path, deadline: Instant) -> HashMap<String, PathBuf> {
    // Each line starts on a fresh line: a profile that prints without a newline (iTerm2's
    // shell integration) must not swallow the first answer.
    let end = shell_end();
    let script = format!(
        r#"for w in "$@"; do unset -f "$w" 2>/dev/null; unalias "$w" 2>/dev/null; printf '\nfleet:path:%s:%s\n' "$w" "$(command -v "$w" 2>/dev/null)"; done; printf '\n%s\n' '{end}'"#
    );
    let Some(got) = login_shell(&script, names, dir)
        .and_then(|mut cmd| capture(&mut cmd, deadline, Some(&end)))
    else {
        return HashMap::new();
    };
    parse_shell_paths(&got.out, names)
}

/// The line that ends the login shell's answer (this process's id keeps a profile from printing it).
fn shell_end() -> String {
    format!("fleet:end:{}", std::process::id())
}

/// `fleet:path:<name>:<what command -v said>` lines → the names answered with an executable
/// file (a builtin or keyword answers with its bare name, a missing one with nothing). A profile
/// may print too; only complete lines that start with these, for the names asked, count.
fn parse_shell_paths(out: &str, names: &[&str]) -> HashMap<String, PathBuf> {
    out.split_inclusive('\n')
        .filter(|l| l.ends_with('\n'))
        .filter_map(|l| {
            l.trim_end_matches(['\r', '\n'])
                .strip_prefix("fleet:path:")?
                .split_once(':')
        })
        .filter(|(name, _)| names.contains(name))
        .map(|(name, p)| (name.to_string(), PathBuf::from(p)))
        .filter(|(_, p)| p.is_absolute() && crate::core::tools::is_executable(p))
        .collect()
}

/// The first line of `<path> --version`, run as argv (no shell) with no input: trimmed, at most
/// 200 characters. `None` when it fails, prints nothing, is not an absolute path (a relative one
/// would run from fleet's directory, not the spawn's) or takes past `deadline` (then it is
/// killed, with whatever it started).
pub fn version_line(path: &Path, deadline: Instant) -> Option<String> {
    if !path.is_absolute() {
        return None;
    }
    let mut cmd = Command::new(path);
    cmd.arg("--version");
    // A script's interpreter (`#!/usr/bin/env node`) is looked for next to it first: npm under
    // nvm installs `codex` beside the node that runs it, on a PATH fleet itself may not have.
    let mut dirs: Vec<PathBuf> = Vec::new();
    if !crate::core::tools::path_only()
        && let Some(parent) = path.parent().filter(|p| p.is_absolute())
    {
        dirs.push(parent.to_path_buf());
    }
    let search = crate::core::tools::search_path();
    dirs.extend(std::env::split_paths(&search));
    if let Ok(p) = std::env::join_paths(dirs) {
        cmd.env("PATH", p);
    }
    let got = capture(&mut cmd, deadline, None).filter(|g| g.done)?;
    let line = got.out.lines().next()?.trim();
    (!line.is_empty()).then(|| line.chars().take(200).collect())
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
    /// Codex: a child thread's rollout (`session_meta.source.subagent`: `spawn_agent`,
    /// `/review`), written by its parent's process, never a process's own by cwd.
    pub child: bool,
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
    let child = h == Harness::Codex && meta.get("source").and_then(|s| s.get("subagent")).is_some();
    Some(TranscriptRef {
        path: path.to_path_buf(),
        session_id,
        cwd,
        created_ms: created,
        mtime_ms: mtime,
        child,
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

/// A Codex / pi session's transcript found by its id alone, wherever it lives (`root` is
/// [`codex_home`] / [`pi_home`]): for a session whose process discovery could not pair with
/// one (it moved to another directory). Codex names files `rollout-<stamp>-<id>.jsonl`, pi
/// `<stamp>_<id>.jsonl`; the most recently written match wins.
pub fn find_transcript(h: Harness, root: &Path, session_id: &str) -> Option<PathBuf> {
    if session_id.is_empty() || session_id.contains(['/', '\\']) {
        return None;
    }
    let sessions = root.join("sessions");
    let candidates: Vec<PathBuf> = match h {
        Harness::Codex => {
            let suffix = format!("-{session_id}.jsonl");
            subdirs(&sessions)
                .iter()
                .flat_map(|y| subdirs(y))
                .flat_map(|m| subdirs(&m))
                .flat_map(|d| jsonl_in(&d).collect::<Vec<_>>())
                .filter(|f| f.to_string_lossy().ends_with(&suffix))
                .collect()
        }
        Harness::Pi => {
            let suffix = format!("_{session_id}.jsonl");
            subdirs(&sessions)
                .iter()
                .flat_map(|d| jsonl_in(d).collect::<Vec<_>>())
                .filter(|f| f.to_string_lossy().ends_with(&suffix))
                .collect()
        }
        Harness::Claude => Vec::new(),
    };
    candidates.into_iter().max_by_key(|p| mtime_ms(p))
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
                .filter(|t| t.cwd == cwd && !t.child && !taken.contains(t.path.as_path()))
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
    /// Codex: `effort` of the last `turn_context`, so a change made mid-session (the `/model`
    /// picker's effort step) shows from the next turn on. pi: not read.
    pub effort: Option<String>,
    /// The `timestamp` of the entry `model` was read from, as written.
    pub model_at: Option<String>,
    pub context: Option<ContextUsage>,
    /// Codex: a `busy` turn's last command asked to leave the sandbox
    /// (`sandbox_permissions: "require_escalated"`) and has no output yet — most likely an
    /// approval prompt on screen (Codex records none in the transcript). Only a hint: a
    /// command approved earlier for the session runs without one. See [`codex_screen_waits_for`].
    pub escalation_pending: bool,
}

/// The last `max` bytes of a file, starting at a line boundary.
fn tail(path: &Path, max: u64) -> Option<String> {
    tail_at(path, max).map(|(_, text)| text)
}

/// [`tail`], with the byte offset its text starts at: everything before it is whole lines.
fn tail_at(path: &Path, max: u64) -> Option<(u64, String)> {
    let mut f = std::fs::File::open(path).ok()?;
    let len = f.metadata().ok()?.len();
    let start = len.saturating_sub(max);
    f.seek(SeekFrom::Start(start)).ok()?;
    let mut buf = Vec::new();
    f.read_to_end(&mut buf).ok()?;
    if start == 0 {
        return Some((0, String::from_utf8_lossy(&buf).into_owned()));
    }
    // The first line is almost certainly cut in half.
    let cut = buf
        .iter()
        .position(|&b| b == b'\n')
        .map_or(buf.len(), |i| i + 1);
    Some((
        start + cut as u64,
        String::from_utf8_lossy(&buf[cut..]).into_owned(),
    ))
}

/// How much of a transcript [`state`] reads: status, approvals and context are all near the end.
const STATE_TAIL: u64 = 512 * 1024;
/// How far back from the end a Codex rollout is searched for its last `turn_context` when the
/// tail holds none (a turn that wrote more than [`STATE_TAIL`] since, e.g. large tool
/// outputs). The same bound as the widest tail Claude's context read takes.
const TURN_CONTEXT_LOOKBACK: u64 = 8 * 1024 * 1024;

pub fn state(h: Harness, path: &Path) -> State {
    let Some((from, text)) = tail_at(path, STATE_TAIL) else {
        return State {
            status: "idle".into(),
            ..Default::default()
        };
    };
    match h {
        Harness::Codex => {
            // The model and effort live only in `turn_context`, written once per turn: when
            // the turn has outgrown the tail, take them from the last one before it.
            let seed = if text.lines().any(|l| turn_context(l).is_some()) {
                None
            } else {
                let len = std::fs::metadata(path).map_or(0, |m| m.len());
                last_turn_context_before(path, from, len.saturating_sub(TURN_CONTEXT_LOOKBACK))
            };
            codex_state_from(seed.as_ref(), &text)
        }
        Harness::Pi => pi_state(&text),
        Harness::Claude => State::default(),
    }
}

/// A Codex `turn_context` entry (its `payload` and `timestamp`); a cheap substring check first,
/// as most lines in a rollout are not one.
fn turn_context(line: &str) -> Option<Value> {
    if !line.contains("\"turn_context\"") {
        return None;
    }
    let v = serde_json::from_str::<Value>(line).ok()?;
    (v.get("type").and_then(Value::as_str) == Some("turn_context") && v.get("payload").is_some())
        .then_some(v)
}

/// The last `turn_context` entry that ends before byte `end` and starts at or after
/// byte `floor`, reading backwards in chunks and stopping at the first one found: the read
/// costs only the distance to that line, never more than `end - floor`.
fn last_turn_context_before(path: &Path, end: u64, floor: u64) -> Option<Value> {
    const CHUNK: u64 = 256 * 1024;
    let mut f = std::fs::File::open(path).ok()?;
    let mut pos = end;
    // The start of a line whose beginning is in an earlier chunk: carried until it is whole.
    let mut carry: Vec<u8> = Vec::new();
    while pos > floor {
        let start = pos.saturating_sub(CHUNK).max(floor);
        f.seek(SeekFrom::Start(start)).ok()?;
        let mut buf = Vec::with_capacity((pos - start) as usize + carry.len());
        f.by_ref().take(pos - start).read_to_end(&mut buf).ok()?;
        buf.extend_from_slice(&carry);
        // Before the first newline is a cut line, unless this chunk starts the file.
        let whole_from = if start == 0 {
            0
        } else {
            match buf.iter().position(|&b| b == b'\n') {
                Some(i) => i + 1,
                None => buf.len(),
            }
        };
        for line in buf[whole_from..].split(|&b| b == b'\n').rev() {
            if let Some(p) = std::str::from_utf8(line).ok().and_then(turn_context) {
                return Some(p);
            }
        }
        carry = buf[..whole_from].to_vec();
        pos = start;
    }
    None
}

/// Apply one `turn_context` entry: its payload's model, with the entry's `timestamp`
/// (`model_at`), and its effort (`null`, the model's default, clears an earlier one — each
/// turn records its own).
fn apply_turn_context(st: &mut State, v: &Value) {
    let Some(p) = v.get("payload") else {
        return;
    };
    if let Some(m) = p.get("model").and_then(Value::as_str) {
        st.model = Some(m.to_string());
        st.model_at = stamp(v);
    }
    st.effort = p
        .get("effort")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|e| !e.is_empty())
        .map(str::to_string);
}

#[cfg(test)]
fn codex_state(text: &str) -> State {
    codex_state_from(None, text)
}

/// A Codex rollout's state from `text` (its tail), starting from `seed`: the last `turn_context`
/// before the tail, when the tail has none of its own.
fn codex_state_from(seed: Option<&Value>, text: &str) -> State {
    let mut st = State {
        status: "idle".into(),
        ..Default::default()
    };
    if let Some(v) = seed {
        apply_turn_context(&mut st, v);
    }
    let mut window = None;
    let mut used = None;
    // Tool calls that asked to leave the sandbox and have no output yet.
    let mut escalations: HashSet<String> = HashSet::new();
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
        let call_id = || {
            p.and_then(|p| p.get("call_id"))
                .and_then(Value::as_str)
                .map(String::from)
        };
        match (kind, ptype) {
            ("response_item", "function_call" | "custom_tool_call") => {
                if p.is_some_and(asks_escalation)
                    && let Some(id) = call_id()
                {
                    escalations.insert(id);
                }
            }
            ("response_item", "function_call_output" | "custom_tool_call_output") => {
                if let Some(id) = call_id() {
                    escalations.remove(&id);
                }
            }
            ("event_msg", "task_started" | "task_complete" | "turn_aborted") => escalations.clear(),
            _ => {}
        }
        match (kind, ptype) {
            ("turn_context", _) => apply_turn_context(&mut st, &v),
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
    st.escalation_pending = st.status == "busy" && !escalations.is_empty();
    st
}

/// A Codex tool call asking to run outside the sandbox: its `sandbox_permissions` argument is
/// `"require_escalated"`. Read from the arguments themselves, never from text inside them — a
/// command that merely mentions the words (`rg 'sandbox_permissions: require_escalated'`) is
/// not a request.
///
/// - `function_call`: `arguments` is JSON; the top-level field decides.
/// - `custom_tool_call` (`exec`, Codex's code mode): `input` is JavaScript; an object key
///   `sandbox_permissions` followed by `:` and the string `"require_escalated"`, with string
///   literals and comments read as data.
fn asks_escalation(payload: &Value) -> bool {
    const KEY: &str = "sandbox_permissions";
    const ESCALATED: &str = "require_escalated";
    if let Some(args) = payload.get("arguments").and_then(Value::as_str) {
        return serde_json::from_str::<Value>(args)
            .ok()
            .and_then(|a| a.get(KEY).and_then(Value::as_str).map(|v| v == ESCALATED))
            .unwrap_or(false);
    }
    let Some(js) = payload.get("input").and_then(Value::as_str) else {
        return false;
    };
    js_tokens(js).windows(3).any(|w| {
        matches!(&w[0], JsTok::Word(k) if *k == KEY)
            || matches!(&w[0], JsTok::Str(k) if k == KEY)
    } && w[1] == JsTok::Punct(':')
        && w[2] == JsTok::Str(ESCALATED.into()))
}

/// Just enough JavaScript tokenizing to tell code from data: words, string literals (their
/// contents, escapes resolved) and single punctuation; whitespace and comments are skipped.
#[derive(Debug, PartialEq)]
enum JsTok<'a> {
    Word(&'a str),
    Str(String),
    Punct(char),
}

fn js_tokens(src: &str) -> Vec<JsTok<'_>> {
    let mut out = Vec::new();
    let mut it = src.char_indices().peekable();
    while let Some((i, c)) = it.next() {
        match c {
            c if c.is_whitespace() => {}
            '/' if it.peek().is_some_and(|(_, n)| *n == '/') => {
                for (_, n) in it.by_ref() {
                    if n == '\n' {
                        break;
                    }
                }
            }
            '/' if it.peek().is_some_and(|(_, n)| *n == '*') => {
                it.next();
                let mut star = false;
                for (_, n) in it.by_ref() {
                    if star && n == '/' {
                        break;
                    }
                    star = n == '*';
                }
            }
            '"' | '\'' | '`' => {
                let mut text = String::new();
                while let Some((_, n)) = it.next() {
                    match n {
                        '\\' => {
                            if let Some((_, e)) = it.next() {
                                text.push(e);
                            }
                        }
                        n if n == c => break,
                        n => text.push(n),
                    }
                }
                out.push(JsTok::Str(text));
            }
            c if c.is_alphanumeric() || c == '_' || c == '$' => {
                let mut end = i + c.len_utf8();
                while let Some((j, n)) = it.peek().copied() {
                    if n.is_alphanumeric() || n == '_' || n == '$' {
                        end = j + n.len_utf8();
                        it.next();
                    } else {
                        break;
                    }
                }
                out.push(JsTok::Word(&src[i..end]));
            }
            c => out.push(JsTok::Punct(c)),
        }
    }
    out
}

/// What a Codex screen (`tmux capture-pane -p`) is waiting on, when it shows one of its
/// approval prompts: `exec approval` (run a command), `patch approval` (edits),
/// `permissions approval`, `network approval`, `terminal input approval`, `mcp approval`.
///
/// Codex 0.162 writes no approval event to its transcript, so the screen is the only place
/// the prompt shows. Codex draws it at the bottom, in place of the composer: a head (its title,
/// then what it is about), then numbered answers (`› 1. Yes, proceed (y)` …), then at most a
/// footer (`Press enter to confirm or esc to cancel`). So only the bottom-most block of answers
/// counts, and only when nothing but its own wrapped lines, the footer and blank lines follow
/// it: the composer (`› …`), a working indicator or any later conversation item (`• …`,
/// `✔ …`) means the dialog on screen was quoted, or already answered. The head is joined
/// into one line before the title is looked for, so a title wrapped in a narrow pane still
/// counts.
pub fn codex_screen_waits_for(screen: &str) -> Option<&'static str> {
    const TITLES: [(&str, &str); 6] = [
        (
            "Would you like to run the following command?",
            "exec approval",
        ),
        (
            "Would you like to make the following edits?",
            "patch approval",
        ),
        (
            "Would you like to grant these permissions?",
            "permissions approval",
        ),
        (
            "Do you want to approve network access to",
            "network approval",
        ),
        ("Would you like to send input to", "terminal input approval"),
        ("needs your approval.", "mcp approval"),
    ];
    let lines: Vec<&str> = screen.lines().collect();
    let last = lines.iter().rposition(|l| is_answer_line(l))?;
    // Below the answers: only their own wrapped text, the footer, blank lines.
    if lines[last + 1..].iter().any(|l| starts_an_item(l)) {
        return None;
    }
    // Up over the answers and their wrapped lines to the first answer.
    let mut first = last;
    for (i, l) in lines[..last].iter().enumerate().rev() {
        if is_answer_line(l) {
            first = i;
        } else if !l.starts_with("   ") || l.trim().is_empty() {
            // Not a wrapped answer line: the answers start below it.
            break;
        }
    }
    // The head: everything between the last conversation item and the answers.
    let start = lines[..first]
        .iter()
        .rposition(|l| starts_an_item(l))
        .map_or(0, |i| i + 1);
    let head = lines[start..first]
        .iter()
        .map(|l| l.trim())
        .filter(|l| !l.is_empty())
        .collect::<Vec<_>>()
        .join(" ");
    TITLES
        .iter()
        .find(|(t, _)| head.contains(t))
        .map(|(_, what)| *what)
}

/// A line that starts something of its own: Codex draws every conversation item, the composer
/// and the working indicator from the first column (`• …`, `› …`, `✔ …`), and everything
/// inside the approval prompt indented — its selected answer (`› 1. …`) aside.
fn starts_an_item(line: &str) -> bool {
    line.starts_with(|c: char| !c.is_whitespace()) && !is_answer_line(line)
}

/// `› 1. Yes, proceed (y)` / `  3. No, and tell Codex what to do differently (esc)`.
fn is_answer_line(line: &str) -> bool {
    let l = line.trim_start().trim_start_matches('›').trim_start();
    let digits = l.chars().take_while(char::is_ascii_digit).count();
    digits > 0
        && l[digits..]
            .strip_prefix(". ")
            .is_some_and(|rest| rest.starts_with("Yes,") || rest.starts_with("No,"))
}

/// A transcript entry's `timestamp`, as written.
fn stamp(v: &Value) -> Option<String> {
    v.get("timestamp")
        .and_then(Value::as_str)
        .map(str::to_string)
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
                st.model_at = stamp(&v);
            }
            Some("message") => {
                let Some(m) = v.get("message") else { continue };
                let role = m.get("role").and_then(Value::as_str).unwrap_or("");
                st.status = match role {
                    "user" | "toolResult" | "bashExecution" => "busy",
                    "assistant" => {
                        if let Some(model) = m.get("model").and_then(Value::as_str) {
                            st.model = Some(model.to_string());
                            st.model_at = stamp(&v);
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
            "claude --resume=s1"
        );
        // An id that reads like a flag stays the id.
        assert_eq!(
            Harness::Claude.resume_command("claude", None, "--help"),
            "claude --resume=--help"
        );
        assert_eq!(
            Harness::Codex.resume_command("codex", None, "--help"),
            "codex resume -- --help"
        );
        assert_eq!(
            Harness::Codex.resume_command("codex", Some("gpt-6"), "s1"),
            "codex --model 'gpt-6' resume -- s1"
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
            child: false,
        }
    }

    #[test]
    fn a_codex_session_is_never_paired_with_its_child_agents_rollouts() {
        // `spawn_agent` and `/review` write child rollouts in the parent's cwd, created
        // after it and written to while the parent waits on them.
        let mut p = proc_(1, "/w", 1_000_000);
        p.harness = Harness::Codex;
        p.argv = argv("codex");
        let child = |id: &str, mtime: i64| TranscriptRef {
            child: true,
            ..tref(id, "/w", 1_200_000, mtime)
        };
        let ts = vec![
            tref("parent", "/w", 1_000_500, 1_300_000),
            child("spawned", 1_900_000),
            child("review", 1_800_000),
        ];
        assert_eq!(assign(&[&p], &ts)[&1].session_id, "parent");
        // Before the parent's first prompt there's no rollout of its own: it stays unpaired.
        assert!(assign(&[&p], &ts[1..]).is_empty());
        // Resumed by its id, a child's rollout is still that process's own.
        let mut resumed = proc_(2, "/w", 2_000_000);
        resumed.harness = Harness::Codex;
        resumed.argv = argv("codex resume spawned");
        assert_eq!(assign(&[&resumed], &ts)[&2].session_id, "spawned");
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
        assert_eq!(st.effort, None, "a turn_context without effort");
        assert_eq!(st.model_at, None, "a turn_context without a timestamp");
    }

    #[test]
    fn codex_effort_is_the_last_turn_contexts() {
        // A live rollout, trimmed: xhigh, then changed to high mid-session.
        let rollout = include_str!("../../../../testdata/codex/effort-change-rollout.jsonl");
        let st = codex_state(rollout);
        assert_eq!(st.effort.as_deref(), Some("high"));
        assert_eq!(st.model.as_deref(), Some("gpt-6-luna"));
        // Before the change's turn_context, the earlier turn's effort still holds.
        let before: Vec<&str> = rollout.lines().take(7).collect();
        assert_eq!(
            codex_state(&before.join("\n")).effort.as_deref(),
            Some("xhigh")
        );
        // A later turn without one (the model's default) is no longer known.
        let unset = r#"{"type":"turn_context","payload":{"model":"gpt-6-luna","effort":null}}"#;
        assert_eq!(codex_state(&format!("{rollout}{unset}\n")).effort, None);
    }

    /// The effort-change rollout with `filler` bytes of tool output between the last turn's
    /// `turn_context` and its end, written as lines of about `line` bytes each.
    fn rollout_with_filler(filler: usize, line: usize, tail_extra: &str) -> String {
        let rollout = include_str!("../../../../testdata/codex/effort-change-rollout.jsonl");
        let lines: Vec<&str> = rollout.lines().collect();
        // Lines 0..9 end with the last turn's turn_context (high); the rest close the turn.
        let mut out = lines[..9].join("\n") + "\n";
        // A tool output that quotes a turn_context line is data, not a turn_context.
        let decoy = r#"{"type":"turn_context","payload":{"model":"decoy","effort":"low"}}"#;
        let mut written = 0;
        let mut i = 0;
        while written < filler {
            let output = format!("{decoy} {}", "x".repeat(line));
            let l = serde_json::json!({"type": "response_item", "payload": {
                "type": "function_call_output", "call_id": format!("c{i}"), "output": output}})
            .to_string();
            written += l.len() + 1;
            out.push_str(&l);
            out.push('\n');
            i += 1;
        }
        out.push_str(tail_extra);
        out + &lines[9..].join("\n") + "\n"
    }

    fn state_of(text: &str) -> State {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("rollout.jsonl");
        std::fs::write(&p, text).unwrap();
        state(Harness::Codex, &p)
    }

    #[test]
    fn codex_effort_survives_a_turn_longer_than_the_tail() {
        // Over 512 KiB of tool output since the turn_context, in lines that straddle the
        // backwards scan's chunks.
        let text = rollout_with_filler(700 * 1024, 300 * 1024, "");
        let st = state_of(&text);
        assert_eq!(st.effort.as_deref(), Some("high"));
        assert_eq!(st.model.as_deref(), Some("gpt-6-luna"));
        assert_eq!(st.status, "idle");
        let c = st.context.unwrap();
        assert_eq!(c.used, 51680);
        assert_eq!(c.model.as_deref(), Some("gpt-6-luna"));
        // Small lines too.
        let st = state_of(&rollout_with_filler(600 * 1024, 4 * 1024, ""));
        assert_eq!(st.effort.as_deref(), Some("high"));

        // A turn_context inside the tail still wins, `null` effort included.
        let unset = r#"{"type":"turn_context","payload":{"model":"gpt-6-luna","effort":null}}"#;
        let st = state_of(&rollout_with_filler(
            700 * 1024,
            300 * 1024,
            &format!("{unset}\n"),
        ));
        assert_eq!(st.effort, None);
        assert_eq!(st.model.as_deref(), Some("gpt-6-luna"));

        // Beyond the lookback bound, unknown rather than a whole-file read.
        let far = rollout_with_filler((TURN_CONTEXT_LOOKBACK + 64 * 1024) as usize, 512 * 1024, "");
        let st = state_of(&far);
        assert_eq!((st.effort, st.model), (None, None));
    }

    // `model_at`: the timestamp of the entry the model was read from — Codex's last
    // `turn_context`, pi's last reply or model change.
    #[test]
    fn the_model_comes_with_the_time_of_its_entry() {
        let ctx = |at: &str, m: &str| {
            format!(r#"{{"timestamp":"{at}","type":"turn_context","payload":{{"model":"{m}"}}}}"#)
        };
        let later = r#"{"timestamp":"2026-01-02T03:09:00.000Z","type":"event_msg","payload":{"type":"task_complete"}}"#;
        let st = codex_state(
            &[
                ctx("2026-01-02T03:04:05.000Z", "gpt-6"),
                ctx("2026-01-02T03:08:00.000Z", "gpt-6.1"),
                later.to_string(),
            ]
            .join("\n"),
        );
        assert_eq!(st.model.as_deref(), Some("gpt-6.1"));
        assert_eq!(st.model_at.as_deref(), Some("2026-01-02T03:08:00.000Z"));
        let pi = [
            r#"{"type":"model_change","timestamp":"2026-01-02T03:04:05.000Z","modelId":"a/b"}"#,
            r#"{"type":"message","timestamp":"2026-01-02T03:05:00.000Z","message":{"role":"assistant","stopReason":"stop","model":"a/c"}}"#,
            r#"{"type":"message","timestamp":"2026-01-02T03:06:00.000Z","message":{"role":"user"}}"#,
        ];
        let st = pi_state(&pi.join("\n"));
        assert_eq!(st.model.as_deref(), Some("a/c"));
        assert_eq!(st.model_at.as_deref(), Some("2026-01-02T03:05:00.000Z"));
    }

    fn exec_asks(js: &str) -> bool {
        asks_escalation(&serde_json::json!({"type": "custom_tool_call", "input": js}))
    }

    // Codex 0.162 records no approval event: the only trace is a tool call that asked to leave
    // the sandbox and has no output yet.
    #[test]
    fn a_pending_escalated_codex_call_is_flagged() {
        let ev = |t: &str| format!(r#"{{"type":"event_msg","payload":{{"type":"{t}"}}}}"#);
        let exec = |id: &str, js: &str| {
            serde_json::json!({"type":"response_item","payload":{
                "type":"custom_tool_call","name":"exec","call_id":id,"input":js}})
            .to_string()
        };
        let out = |id: &str| {
            serde_json::json!({"type":"response_item","payload":{
                "type":"custom_tool_call_output","call_id":id,"output":"done"}})
            .to_string()
        };
        let asks = r#"text(await tools.exec_command({cmd:"touch x",sandbox_permissions:"require_escalated",justification:"May I?"}));"#;
        let plain = r#"text(await tools.exec_command({cmd:"ls"}));"#;
        let st = codex_state(&[ev("task_started"), exec("c1", asks)].join("\n"));
        assert_eq!((st.status.as_str(), st.escalation_pending), ("busy", true));
        // Answered (approved or denied): output arrives.
        let st = codex_state(&[ev("task_started"), exec("c1", asks), out("c1")].join("\n"));
        assert!(!st.escalation_pending);
        let st = codex_state(&[ev("task_started"), exec("c2", plain)].join("\n"));
        assert!(!st.escalation_pending);
        // The function-call form, with JSON arguments.
        let fc = serde_json::json!({"type":"response_item","payload":{"type":"function_call",
            "name":"exec_command","call_id":"c3",
            "arguments":"{\"cmd\":\"git fetch\",\"sandbox_permissions\":\"require_escalated\"}"}})
        .to_string();
        assert!(codex_state(&[ev("task_started"), fc.clone()].join("\n")).escalation_pending);
        // An interrupted turn leaves nothing pending.
        let st = codex_state(&[ev("task_started"), fc, ev("turn_aborted")].join("\n"));
        assert!(!st.escalation_pending);
        // Words inside a command are data, not a request: an `rg` for the very field.
        let rg = r#"text(await tools.exec_command({cmd:"rg 'sandbox_permissions: require_escalated' ."}));"#;
        let st = codex_state(&[ev("task_started"), exec("c4", rg)].join("\n"));
        assert!(!st.escalation_pending);
        let rg_fc = serde_json::json!({"type":"response_item","payload":{"type":"function_call",
            "name":"exec_command","call_id":"c5",
            "arguments": serde_json::json!({"cmd": "rg '\"sandbox_permissions\": \"require_escalated\"' ."}).to_string()}})
        .to_string();
        assert!(!codex_state(&[ev("task_started"), rg_fc].join("\n")).escalation_pending);
        // A comment saying it is not one either; a quoted key with spacing is.
        assert!(!exec_asks(
            "// sandbox_permissions: \"require_escalated\"\nrun()"
        ));
        assert!(exec_asks(
            "f({ 'sandbox_permissions' : 'require_escalated' })"
        ));
        assert!(!exec_asks("f({ sandbox_permissions: 'use_default' })"));
    }

    // Recorded from Codex 0.162 sitting on "Would you like to run the following command?":
    // the transcript says only `busy` (no approval event), with the escalated call pending.
    #[test]
    fn a_recorded_codex_approval_is_busy_in_the_transcript_with_an_escalation_pending() {
        let rollout = include_str!("../../../../testdata/codex/exec-approval-rollout.jsonl");
        let st = codex_state(rollout);
        assert_eq!(st.status, "busy");
        assert_eq!(st.waiting_for, None);
        assert!(st.escalation_pending);
    }

    #[test]
    fn codex_approval_prompts_are_read_off_the_screen() {
        let screen = include_str!("../../../../testdata/codex/exec-approval-pane.txt");
        assert_eq!(codex_screen_waits_for(screen), Some("exec approval"));
        // The same prompt in narrow panes: at 40 columns the title wraps onto two lines.
        for narrow in [
            include_str!("../../../../testdata/codex/exec-approval-pane-60.txt"),
            include_str!("../../../../testdata/codex/exec-approval-pane-40.txt"),
        ] {
            assert_eq!(codex_screen_waits_for(narrow), Some("exec approval"));
        }
        // Answered, and the turn went on: the dialog may still be on screen above it.
        let answered = format!(
            "{screen}\n✔ You approved codex to run touch approval-probe.txt this time\n\n\
             • Ran touch approval-probe.txt\n\n› Ask Codex to do anything\n\n  gpt-6 · 10% used\n"
        );
        assert_eq!(codex_screen_waits_for(&answered), None);
        let working = format!("{screen}\n• Working (3s • esc to interrupt)\n");
        assert_eq!(codex_screen_waits_for(&working), None);
        // A whole dialog pasted into the conversation, as the user's message.
        let pasted = "› Would you like to run the following command?\n\n\
                      \x20 $ touch x\n\n\
                      \x20 › 1. Yes, proceed (y)\n\
                      \x20 2. No, and tell Codex what to do differently (esc)\n\n\
                      • That looks like an approval prompt.\n\n\
                      › Ask Codex to do anything\n\n  gpt-6 · 10% used\n";
        assert_eq!(codex_screen_waits_for(pasted), None);
        // Answered: the prompt is gone, the turn goes on.
        let after = "• Ran touch approval-probe.txt\n\n› Ask Codex to do anything\n";
        assert_eq!(codex_screen_waits_for(after), None);
        // The same title quoted in the conversation, with the composer below it, is no prompt.
        let quoted = "• Codex asks \"Would you like to run the following command?\" first.\n\n\
                      › Ask Codex to do anything\n\n  gpt-6 medium · 0% used\n";
        assert_eq!(codex_screen_waits_for(quoted), None);
        let edits = "  Would you like to make the following edits?\n\n  › 1. Yes, proceed (y)\n\
                       2. No, and tell Codex what to do differently (esc)\n";
        assert_eq!(codex_screen_waits_for(edits), Some("patch approval"));
        assert_eq!(codex_screen_waits_for(""), None);
        assert!(is_answer_line("› 1. Yes, proceed (y)"));
        assert!(is_answer_line(
            "  3. No, and tell Codex what to do differently (esc)"
        ));
        assert!(!is_answer_line("1. Yesterday's notes"));
        assert!(!is_answer_line("  2. Keep current model"));
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
    fn claude_default_model_prefers_the_env_then_user_settings() {
        let dir = tempfile::tempdir().unwrap();
        let settings = dir.path().join("settings.json");
        // Nothing configured: unknown, plain "Default".
        assert_eq!(claude_default_model(None, dir.path()), None);
        assert_eq!(claude_models(None)[0].label, "Default");

        std::fs::write(
            &settings,
            r#"{"env":{"ANTHROPIC_API_KEY":"sk-not-read"},"model":"opus[1m]"}"#,
        )
        .unwrap();
        let m = claude_default_model(None, dir.path());
        assert_eq!(m.as_deref(), Some("opus[1m]"));
        assert_eq!(claude_models(m.as_deref())[0].label, "Default (opus[1m])");
        assert_eq!(claude_models(m.as_deref())[0].id, "");

        // ANTHROPIC_MODEL wins over the settings; an empty one doesn't count.
        assert_eq!(
            claude_default_model(Some("claude-sonnet-5-5"), dir.path()).as_deref(),
            Some("claude-sonnet-5-5")
        );
        assert_eq!(
            claude_default_model(Some(" "), dir.path()).as_deref(),
            Some("opus[1m]")
        );
    }

    /// (id, efforts, default_effort) of each entry.
    fn efforts_of(models: &[ModelOption]) -> Vec<(String, Vec<String>, Option<String>)> {
        models
            .iter()
            .map(|m| (m.id.clone(), m.efforts.clone(), m.default_effort.clone()))
            .collect()
    }

    fn words(w: &[&str]) -> Vec<String> {
        w.iter().map(|s| s.to_string()).collect()
    }

    // Claude's levels are fleet's table: every model low to max, starting at medium (Fable at
    // high); the default entry takes the levels of the model it names, an alias resolved.
    #[test]
    fn claude_models_carry_their_effort_levels() {
        let all = words(&["low", "medium", "high", "xhigh", "max"]);
        let level = |l: &str| Some(l.to_string());
        assert_eq!(
            efforts_of(&claude_models(None)),
            [
                ("".into(), all.clone(), None),
                ("claude-fable-5-1".into(), all.clone(), level("high")),
                ("claude-opus-5-5".into(), all.clone(), level("medium")),
                ("claude-sonnet-5-5".into(), all.clone(), level("medium")),
                ("claude-haiku-5-5".into(), all.clone(), level("medium")),
            ]
        );
        for (default, starts) in [
            ("opus[1m]", level("medium")),
            ("fable", level("high")),
            ("claude-fable-5-1", level("high")),
            ("claude-sonnet-5-5[1m]", level("medium")),
            ("gpt-6", None),
            ("claude-opus-4-1", None),
        ] {
            let d = &claude_models(Some(default))[0];
            assert_eq!(
                (d.efforts.clone(), d.default_effort.clone()),
                (all.clone(), starts),
                "{default}"
            );
        }
        // In `harness list --json`: always there, `[]` and null where none is known.
        let v = serde_json::to_value(&claude_models(None)[1]).unwrap();
        assert_eq!(v["efforts"][4], "max", "{v}");
        assert_eq!(v["default_effort"], "high", "{v}");
        let v = serde_json::to_value(opt("x", "X")).unwrap();
        assert_eq!(
            (v["efforts"].clone(), v["default_effort"].clone()),
            (serde_json::json!([]), Value::Null),
            "{v}"
        );
    }

    /// Each listed Claude model's `default_effort` under `settings` (a `settings.json`) and
    /// `env_level`, by id, the default entry (`default: opus`) first.
    fn claude_starts(settings: &str, env_level: Option<&str>) -> Vec<(String, Option<String>)> {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("settings.json"), settings).unwrap();
        let saved = claude_saved_efforts(dir.path(), env_level);
        claude_models_saved(Some("opus"), &saved)
            .into_iter()
            .map(|m| (m.id, m.default_effort))
            .collect()
    }

    fn starts(levels: [Option<&str>; 5]) -> Vec<(String, Option<String>)> {
        [
            "",
            "claude-fable-5-1",
            "claude-opus-5-5",
            "claude-sonnet-5-5",
            "claude-haiku-5-5",
        ]
        .iter()
        .zip(levels)
        .map(|(id, l)| (id.to_string(), l.map(str::to_string)))
        .collect()
    }

    // `/effort` saves a level per model in Claude Code's settings: that is where a new session
    // on it starts. Only `low` to `xhigh` are saved levels; another word is no level saved.
    #[test]
    fn a_saved_effort_level_is_the_models_default() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(
            claude_saved_efforts(dir.path(), None),
            SavedEfforts::default()
        );
        assert_eq!(
            claude_starts(
                r#"{"env":{"ANTHROPIC_API_KEY":"sk-not-read"},"model":"opus",
                    "modelSettings":{"claude-opus-5-5":{"effortLevel":"xhigh"},
                      "claude-haiku-5-5":{"effortLevel":"turbo"},"claude-sonnet-5-5":{"other":1},
                      "claude-fable-5-1":{"effortLevel":"max"}}}"#,
                None
            ),
            starts([
                Some("xhigh"),
                Some("high"),
                Some("xhigh"),
                Some("medium"),
                Some("medium")
            ])
        );
        for text in ["not json", r#"{"modelSettings":[1]}"#] {
            assert_eq!(
                claude_starts(text, None),
                starts([
                    Some("medium"),
                    Some("high"),
                    Some("medium"),
                    Some("medium"),
                    Some("medium")
                ]),
                "{text}"
            );
        }
    }

    // Claude Code keys a saved level by any spelling of the model — an alias, `[1m]`, a date —
    // and takes the entry keyed by the model's own id over the others, else the first. A key
    // fleet cannot resolve to a listed model is skipped.
    #[test]
    fn a_saved_level_counts_for_the_model_its_key_names() {
        assert_eq!(
            claude_starts(
                r#"{"modelSettings":{
                    "opus[1m]":{"effortLevel":"low"},"claude-opus-5-5":{"effortLevel":"high"},
                    "claude-sonnet-5-5[1m]":{"effortLevel":"low"},"sonnet":{"effortLevel":"xhigh"},
                    "claude-haiku-5-5-20261001":{"effortLevel":"turbo"},
                    "haiku":{"effortLevel":"high"},
                    "us.anthropic.claude-fable-5-1-v1:0":{"effortLevel":"low"}}}"#,
                None
            ),
            starts([
                Some("high"),
                Some("high"),
                Some("high"),
                Some("low"),
                Some("high")
            ])
        );
    }

    // A top-level `effortLevel` Claude Code applies to some models and not others, by lists of
    // its own: a model without a level of its own then has no certain default. One of another
    // word is no level saved.
    #[test]
    fn a_top_level_effort_level_makes_the_default_uncertain() {
        assert_eq!(
            claude_starts(
                r#"{"effortLevel":"low","modelSettings":{"claude-opus-5-5":{"effortLevel":"xhigh"}}}"#,
                None
            ),
            starts([Some("xhigh"), None, Some("xhigh"), None, None])
        );
        assert_eq!(
            claude_starts(r#"{"effortLevel":"max"}"#, None),
            starts([
                Some("medium"),
                Some("high"),
                Some("medium"),
                Some("medium"),
                Some("medium")
            ])
        );
    }

    // `maxEffortLevel` holds the level down: a model's own (the lowest of its entries) over the
    // top-level one, `max` no cap at all.
    #[test]
    fn the_saved_cap_holds_the_default_down() {
        assert_eq!(
            claude_starts(
                r#"{"maxEffortLevel":"low","modelSettings":{
                    "claude-opus-5-5":{"effortLevel":"xhigh","maxEffortLevel":"xhigh"},
                    "opus":{"maxEffortLevel":"high"},
                    "claude-fable-5-1":{"maxEffortLevel":"max"},
                    "claude-sonnet-5-5":{"maxEffortLevel":"turbo"}}}"#,
                None
            ),
            starts([
                Some("high"),
                Some("high"),
                Some("high"),
                Some("low"),
                Some("low")
            ])
        );
    }

    // `CLAUDE_CODE_EFFORT_LEVEL` overrides every saved level: no default is certain.
    #[test]
    fn the_effort_level_variable_makes_every_default_uncertain() {
        let saved = r#"{"modelSettings":{"claude-opus-5-5":{"effortLevel":"xhigh"}}}"#;
        assert_eq!(claude_starts(saved, Some("high")), starts([None; 5]));
        assert_eq!(
            claude_starts(saved, Some(" ")),
            starts([
                Some("xhigh"),
                Some("high"),
                Some("xhigh"),
                Some("medium"),
                Some("medium")
            ])
        );
    }

    // Codex's levels come from its own cache, per model, lowest first by Codex's own order
    // whatever order the cache lists them in, a level it does not name dropped; its default
    // entry takes the configured model's, else every level, lowest first.
    #[test]
    fn codex_models_carry_their_reasoning_levels() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("models_cache.json"),
            r#"{"models":[
              {"slug":"gpt-6-luna","display_name":"GPT-6-Luna","visibility":"list","priority":2,
               "default_reasoning_level":"medium",
               "supported_reasoning_levels":[{"effort":"ultra","description":"a"},{"effort":"low"},{"effort":"turbo"},{"effort":"medium"}]},
              {"slug":"gpt-6-sol","display_name":"GPT-6-Sol","visibility":"list","priority":1,
               "default_reasoning_level":"turbo","supported_reasoning_levels":[{"effort":"max"},{"effort":"minimal"}]},
              {"slug":"gpt-6-old","visibility":"list","priority":3}
            ]}"#,
        )
        .unwrap();
        let level = |l: &str| Some(l.to_string());
        assert_eq!(
            efforts_of(&codex_models(dir.path())),
            [
                (
                    "".into(),
                    words(&["minimal", "low", "medium", "max", "ultra"]),
                    None
                ),
                ("gpt-6-sol".into(), words(&["minimal", "max"]), None),
                (
                    "gpt-6-luna".into(),
                    words(&["low", "medium", "ultra"]),
                    level("medium")
                ),
                ("gpt-6-old".into(), vec![], None),
            ]
        );
        std::fs::write(dir.path().join("config.toml"), "model = \"gpt-6-luna\"\n").unwrap();
        let d = &codex_models(dir.path())[0];
        assert_eq!(d.label, "Default (gpt-6-luna)");
        assert_eq!(
            (d.efforts.clone(), d.default_effort.clone()),
            (words(&["low", "medium", "ultra"]), level("medium"))
        );
    }

    #[test]
    fn claude_default_model_is_unknown_for_unusable_settings() {
        let dir = tempfile::tempdir().unwrap();
        let settings = dir.path().join("settings.json");
        for text in [
            "{not json",
            r#"{"model":42}"#,
            r#"{"model":""}"#,
            r#"{"permissions":{}}"#,
            "[]",
        ] {
            std::fs::write(&settings, text).unwrap();
            assert_eq!(claude_default_model(None, dir.path()), None, "{text}");
        }
        // Oversized: not read at all.
        let pad = " ".repeat(MAX_SETTINGS_BYTES as usize);
        std::fs::write(&settings, format!(r#"{{"model":"opus"}}{pad}"#)).unwrap();
        assert_eq!(claude_default_model(None, dir.path()), None);
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
        assert!(!ts[0].child);
        for (id, source) in [
            (
                "c2",
                r#"{"subagent":{"thread_spawn":{"parent_thread_id":"c1","depth":1}}}"#,
            ),
            ("c3", r#"{"subagent":"review"}"#),
            ("c4", r#""cli""#),
        ] {
            std::fs::write(
                cdir.join(format!("rollout-2026-10-11T21-51-00-{id}.jsonl")),
                format!(
                    r#"{{"type":"session_meta","payload":{{"id":"{id}","timestamp":"2026-10-11T19:51:00.000Z","cwd":"/w/app","source":{source}}}}}"#
                ) + "\n",
            )
            .unwrap();
        }
        let children: Vec<(String, bool)> =
            transcripts(Harness::Codex, &root.join("codex"), &[&cp])
                .into_iter()
                .map(|t| (t.session_id, t.child))
                .collect();
        for (id, child) in [("c1", false), ("c2", true), ("c3", true), ("c4", false)] {
            assert!(children.contains(&(id.into(), child)), "{id}: {children:?}");
        }
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

    fn script(dir: &Path, name: &str, body: &str) -> PathBuf {
        use std::os::unix::fs::PermissionsExt;
        let p = dir.join(name);
        std::fs::write(&p, format!("#!/bin/sh\n{body}\n")).unwrap();
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
        p
    }

    #[test]
    fn a_launcher_path_is_its_executable_file() {
        let dir = tempfile::tempdir().unwrap();
        let exe = script(dir.path(), "agent", "");
        let shown = exe.display().to_string();
        let launchers = [
            format!("{shown} --model 'x'"),
            "./agent".into(),
            dir.path().join("nope").display().to_string(),
            "FOO=1 claude".into(),
            String::new(),
            format!("{}/./agent", dir.path().display()),
        ];
        // Path-shaped launchers only: nothing is asked of a shell or PATH. A relative one has no
        // meaning here (a spawn runs it elsewhere): null, though `./agent` exists in `dir`. A
        // `.` component is dropped.
        let far = Instant::now() + Duration::from_secs(5);
        assert_eq!(
            launcher_paths(&launchers, dir.path(), far),
            [Some(exe.clone()), None, None, None, None, Some(exe.clone())]
        );
        // And a relative path is not run for its version either.
        assert_eq!(version_line(Path::new("agent"), far), None);
    }

    #[test]
    fn a_login_shell_answer_is_cut_at_its_end_and_loses_nothing_before_it() {
        let dir = tempfile::tempdir().unwrap();
        let start = Instant::now();
        // The answers, the end marker, then a job that holds stdout open for 30 s.
        let mut cmd = Command::new("/bin/sh");
        cmd.args([
            "-c",
            "printf 'noise'; printf '\\nfleet:path:a:/bin/sh\\n'; printf '\\nfleet:end:1\\n'; sleep 30 &",
        ])
        .current_dir(dir.path());
        let got = capture(
            &mut cmd,
            start + Duration::from_secs(3),
            Some("fleet:end:1"),
        )
        .unwrap();
        assert!(got.done);
        assert!(
            start.elapsed() < Duration::from_secs(2),
            "{:?}",
            start.elapsed()
        );
        assert_eq!(
            parse_shell_paths(&got.out, &["a"]),
            HashMap::from([("a".to_string(), PathBuf::from("/bin/sh"))])
        );
        // No end marker: at the deadline, what was answered by then is kept, not done.
        let start = Instant::now();
        let mut cmd = Command::new("/bin/sh");
        cmd.args([
            "-c",
            "printf '\\nfleet:path:a:/bin/sh\\nfleet:path:b:/bi'; sleep 30",
        ]);
        let got = capture(
            &mut cmd,
            start + Duration::from_millis(400),
            Some("fleet:end:1"),
        )
        .unwrap();
        assert!(!got.done);
        assert!(
            start.elapsed() < Duration::from_secs(2),
            "{:?}",
            start.elapsed()
        );
        // A line cut short by the deadline does not count.
        assert_eq!(
            parse_shell_paths(&got.out, &["a", "b"]),
            HashMap::from([("a".to_string(), PathBuf::from("/bin/sh"))])
        );
    }

    #[test]
    fn a_probe_reads_at_most_64_kib_and_survives_bad_utf8() {
        let mut cmd = Command::new("/bin/sh");
        cmd.args([
            "-c",
            "printf 'a\\377b\\n'; head -c 200000 /dev/zero | tr '\\0' x",
        ]);
        let got = capture(&mut cmd, Instant::now() + Duration::from_secs(5), None).unwrap();
        assert!(got.done);
        assert!(got.out.starts_with("a\u{fffd}b\n"), "{:?}", &got.out[..10]);
        assert_eq!(got.out.len(), PROBE_OUTPUT_CAP + 2); // U+FFFD is 3 bytes for 1
    }

    #[test]
    fn a_job_left_holding_stdout_after_the_shell_exits_is_killed_at_the_deadline() {
        // The shell exits (and is not the marker's printer) while its background job keeps
        // stdout open: no EOF, so the deadline ends it, and the job must go with the group.
        let arg = "37.123";
        let start = Instant::now();
        let mut cmd = Command::new("/bin/sh");
        cmd.args(["-c", &format!("sleep {arg} & exit 0")]);
        let got = capture(
            &mut cmd,
            start + Duration::from_millis(600),
            Some("fleet:end:1"),
        )
        .unwrap();
        assert!(!got.done);
        assert!(start.elapsed() < Duration::from_secs(5));
        let left = std::process::Command::new("pgrep")
            .args(["-f", &format!("sleep {arg}")])
            .output()
            .unwrap();
        assert!(
            left.stdout.is_empty(),
            "orphan left: {}",
            String::from_utf8_lossy(&left.stdout)
        );
    }

    fn alive(pid: i32) -> bool {
        unsafe { libc::kill(pid, 0) == 0 }
    }

    #[test]
    fn a_timed_out_probe_takes_its_grandchildren_with_it() {
        let dir = tempfile::tempdir().unwrap();
        let pidfile = dir.path().join("pid");
        let wrapper = script(
            dir.path(),
            "wrapper",
            &format!("sleep 30 &\necho $! > '{}'\nwait", pidfile.display()),
        );
        let got = version_line(&wrapper, Instant::now() + Duration::from_millis(500));
        assert_eq!(got, None);
        let pid: i32 = std::fs::read_to_string(&pidfile)
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        // Gone (not just orphaned): the group was killed.
        let until = Instant::now() + Duration::from_secs(2);
        while alive(pid) && Instant::now() < until {
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(!alive(pid), "the wrapper's sleep outlived its probe");
    }

    #[test]
    fn the_login_shell_answer_counts_only_for_files_asked_about() {
        let dir = tempfile::tempdir().unwrap();
        let exe = script(dir.path(), "agent", "");
        let plain = dir.path().join("plain");
        std::fs::write(&plain, "").unwrap();
        let out = format!(
            "Welcome back\nfleet:path:agent:{}\nfleet:path:cd:cd\nfleet:path:gone:\n\
             fleet:path:other:{}\nfleet:path:plain:{}\n",
            exe.display(),
            exe.display(),
            plain.display()
        );
        let got = parse_shell_paths(&out, &["agent", "cd", "gone", "plain"]);
        // A builtin's bare name, nothing found, a file that does not execute, a name not asked.
        assert_eq!(got, HashMap::from([("agent".to_string(), exe)]));
    }

    #[test]
    fn a_version_is_the_first_line_of_its_answer() {
        let dir = tempfile::tempdir().unwrap();
        let far = Instant::now() + Duration::from_secs(5);
        let v = |body: &str| version_line(&script(dir.path(), "tool", body), far);
        assert_eq!(
            v("printf '  2.1.400 (Claude Code) \\nmore\\n'").as_deref(),
            Some("2.1.400 (Claude Code)")
        );
        // Its arguments are `--version` alone.
        assert_eq!(v(r#"echo "$# $1""#).as_deref(), Some("1 --version"));
        let long = v("printf 'é%.0s' $(seq 300)").unwrap();
        assert_eq!(long.chars().count(), 200);
        assert_eq!(v("echo 1.0; exit 3"), None);
        assert_eq!(v("printf '\\n'"), None);
        assert_eq!(v("echo 1.0 >&2"), None);
        assert_eq!(version_line(&dir.path().join("none"), far), None);
    }

    #[test]
    fn a_version_that_does_not_come_in_time_is_none() {
        let dir = tempfile::tempdir().unwrap();
        let t = Instant::now() + Duration::from_millis(300);
        let start = Instant::now();
        assert_eq!(
            version_line(&script(dir.path(), "slow", "sleep 5; echo 1.0"), t),
            None
        );
        // Exits at once, but a child it left behind keeps its stdout open.
        assert_eq!(
            version_line(&script(dir.path(), "bg", "sleep 5 & echo 1.0"), t),
            None
        );
        assert!(
            start.elapsed() < Duration::from_secs(3),
            "{:?}",
            start.elapsed()
        );
    }

    #[test]
    fn transcripts_are_found_by_id_wherever_they_live() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let cdir = root.join("codex/sessions/2026/10/11");
        std::fs::create_dir_all(&cdir).unwrap();
        let cf = cdir.join("rollout-2026-10-11T21-50-53-abc-123.jsonl");
        std::fs::write(&cf, "{}\n").unwrap();
        std::fs::write(cdir.join("rollout-2026-10-11T21-50-53-other.jsonl"), "{}\n").unwrap();
        assert_eq!(
            find_transcript(Harness::Codex, &root.join("codex"), "abc-123"),
            Some(cf)
        );
        let pdir = root
            .join("pi/sessions")
            .join(pi_dir_name("/moved/elsewhere"));
        std::fs::create_dir_all(&pdir).unwrap();
        let pf = pdir.join("2026-10-11T19-50-44-044Z_p1.jsonl");
        std::fs::write(&pf, "{}\n").unwrap();
        assert_eq!(
            find_transcript(Harness::Pi, &root.join("pi"), "p1"),
            Some(pf)
        );
        assert_eq!(find_transcript(Harness::Pi, &root.join("pi"), "p2"), None);
        assert_eq!(
            find_transcript(Harness::Pi, &root.join("pi"), "../p1"),
            None
        );
        assert_eq!(find_transcript(Harness::Claude, root, "p1"), None);
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
