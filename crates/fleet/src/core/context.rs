//! Context-window usage of a Claude session, read from the tail of its transcript
//! (`~/.claude/projects/<encoded cwd>/<session id>.jsonl`).
//!
//! **What counts as "used".** The last main-thread assistant entry's
//! `message.usage`: `input_tokens + cache_read_input_tokens +
//! cache_creation_input_tokens`. That is the size of the prompt the model was
//! last sent — the same sum Claude Code's own statusline `context_window`
//! percentage uses. Output tokens are left out: the last turn's reply becomes
//! input on the next request, where it is counted then.
//!
//! **Which window.** Transcripts don't record it, so it is inferred. 1M when:
//! - the model id carries the `[1m]` suffix;
//! - the model is natively 1M in Claude Code, with no suffix: Fable, Mythos, and
//!   Opus / Sonnet 5.5 and later ([`native_1m`]);
//! - the prompt is already larger than 200k (it can't fit otherwise);
//! - the user's `settings.json` pins a `[1m]` model of the same family;
//! - Claude Code's `~/.claude.json` has recorded this exact model id as
//!   `<id>[1m]` (`projects.*.lastModelUsage`), i.e. the user runs it with 1M;
//! - another live session on the same model id is past 200k without a `[1m]`
//!   suffix, i.e. the model is natively 1M ([`apply_native_1m`]).
//!
//! 200k otherwise. A 1M session under 200k with none of these signals still
//! reads against 200k — the one known blind spot.
//!
//! **Effort.** The same entry's top-level `effort` (`low` … `max`): the reasoning effort that
//! turn ran at, which follows a mid-session `/effort` from the next reply on. Its
//! `perTurnEffort` is not used: SDK sessions write it as `null` while `effort` is set.
//!
//! **Activity.** What a Busy session is doing now, from the same buffer and reverse pass
//! ([`Activity`]): the latest main-thread `tool_use` with no `tool_result` for its id,
//! phrased `"{tool}: {summary}"` as the chat shows it; `Thinking` when every call is
//! answered and the turn is unfinished; none after `end_turn`, an interrupt, or when the
//! pending call lies beyond the buffer (the read is not widened for it). The walk stops at
//! the message before the latest one, so it parses a few entries, not the whole Turn.
//!
//! **Cost.** Only the tail is read (256 KiB, widened to 2 MiB and 8 MiB if no
//! usable entry is found, or until both it and the Turn's prompt are found when
//! finished subagents need it), and results are cached per `(path, size, mtime)`, so a
//! long-running dashboard re-reads a transcript only when it has grown. When current
//! subagents need the Turn, the same pass also collects their task notifications and
//! `TaskStop` results ([`super::subagents::Lifecycle`]) from the whole buffer it read,
//! and the read keeps widening while a depth-1 subagent has no evidence in the buffer and
//! ended (finished) or last wrote its log (unfinished) before its oldest entry
//! ([`super::subagents::Search`]). That pass doesn't stop at the reply: it parses every
//! lifecycle candidate in the buffer, and every `user` or `attachment` line until the
//! Turn's prompt, even when only unfinished subagents asked for it.

use std::collections::HashMap;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::{LazyLock, Mutex};
use std::time::SystemTime;

use serde::{Deserialize, Serialize};

use super::subagents::Search;

pub const WINDOW_200K: u64 = 200_000;
pub const WINDOW_1M: u64 = 1_000_000;

/// Tail sizes tried in order until one holds a usable assistant entry.
const TAILS: [u64; 3] = [256 * 1024, 2 * 1024 * 1024, 8 * 1024 * 1024];

/// `context` in `fleet list --json`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ContextUsage {
    /// Prompt tokens of the last main-thread request.
    pub used: u64,
    /// Inferred context window: 200000 or 1000000.
    pub window: u64,
    /// `used / window` as a whole percentage (rounded; may exceed 100).
    pub pct: u32,
    /// `message.model` of that entry, e.g. `claude-opus-4-5`.
    pub model: Option<String>,
}

impl ContextUsage {
    pub fn new(used: u64, window: u64, model: Option<String>) -> Self {
        let pct = if window == 0 {
            0
        } else {
            ((used as f64 / window as f64) * 100.0).round() as u32
        };
        Self {
            used,
            window,
            pct,
            model,
        }
    }
}

/// Severity band shared by every view: `<60` calm, `60–85` warn, `>85` hot.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Level {
    Low,
    Warn,
    Hot,
}

pub fn level(pct: u32) -> Level {
    if pct > 85 {
        Level::Hot
    } else if pct >= 60 {
        Level::Warn
    } else {
        Level::Low
    }
}

/// Where a session's transcript lives: `<home>/projects/<encoded cwd>/<sid>.jsonl`,
/// or — when the session moved (worktree, `cd`) — whichever project dir holds it.
pub fn locate_transcript(home: &Path, cwd: Option<&str>, session_id: &str) -> Option<PathBuf> {
    let name = format!("{session_id}.jsonl");
    let root = home.join("projects");
    if let Some(cwd) = cwd {
        let direct = root.join(super::discovery::encode_cwd(cwd)).join(&name);
        if direct.is_file() {
            return Some(direct);
        }
    }
    std::fs::read_dir(&root)
        .ok()?
        .flatten()
        .map(|d| d.path().join(&name))
        .find(|p| p.is_file())
}

type CacheKey = (PathBuf, u64, Option<SystemTime>);
/// Facts from one tail: reply plus the optional current Turn prompt.
#[derive(Debug, Default, Clone)]
struct Raw {
    reply: Option<LastTurn>,
    /// What the session is doing now ([`Activity`]).
    activity: Option<String>,
    turn_start: Option<String>,
    // The latest prompt can have an unusable timestamp. It still ends the search.
    prompt_found: bool,
    /// Subagent lifecycle evidence, collected only with the Turn search.
    lifecycle: super::subagents::Lifecycle,
    /// The first `timestamp` in the buffer, found only with the Turn search.
    oldest: Option<SystemTime>,
    /// The buffer starts at the file's start or is the widest tail: no wider read exists.
    exhausted: bool,
}

impl Raw {
    /// A search that wants the Turn's prompt widens until it is found. A finished depth-1
    /// subagent without evidence here ended before the buffer's oldest entry, so its
    /// notification, written at or after that end, may lie further back. So may the stop of
    /// an unfinished depth-1 subagent without evidence here whose log was last written
    /// before that entry. A buffer without a timestamp widens too.
    fn wants_wider(&self, search: Option<&Search>) -> bool {
        let before_buffer = |id: &str, at: SystemTime| {
            !self.lifecycle.decides(id) && self.oldest.is_none_or(|oldest| oldest > at)
        };
        !self.exhausted
            && search.is_some_and(|search| {
                (search.turn && !self.prompt_found)
                    || search
                        .awaited
                        .iter()
                        .chain(&search.quiet)
                        .any(|(id, at)| before_buffer(id, *at))
            })
    }
}

type CachedTail = (CacheKey, bool, Raw);

/// What the last usable main-thread assistant entry says.
#[derive(Debug, Clone, PartialEq)]
struct LastTurn {
    used: u64,
    model: Option<String>,
    effort: Option<String>,
    /// Its `timestamp`, as written.
    at: Option<String>,
}

/// What a live session's last reply says ([`for_session`]).
#[derive(Debug, Clone, PartialEq)]
pub struct LastReply {
    pub context: ContextUsage,
    /// The reasoning effort it ran at; `None` when it records none.
    pub effort: Option<String>,
    /// Its `timestamp`, as written: when the reply `context.model` came from was written
    /// (`list`'s `model_at`).
    pub at: Option<String>,
}
static CACHE: LazyLock<Mutex<HashMap<PathBuf, CachedTail>>> = LazyLock::new(Default::default);

/// What says a model runs with a 1M window, besides its own id.
#[derive(Debug, Default, Clone)]
pub struct Hints {
    /// Model family the user's `settings.json` pins with `[1m]` (`sonnet[1m]` -> `sonnet`).
    pub settings_1m: Option<String>,
    /// Model ids Claude Code has recorded as `<id>[1m]` in `~/.claude.json`
    /// (`projects.*.lastModelUsage`), per project dir and across all of them.
    pub project_1m: HashMap<String, Vec<String>>,
    pub any_1m: Vec<String>,
}

impl Hints {
    /// Read the hints from the user's Claude config (`settings.json`, `~/.claude.json`).
    pub fn load(home: &Path) -> Self {
        let mut h = Hints {
            settings_1m: settings_1m_model(home),
            ..Default::default()
        };
        let state = std::env::var_os("CLAUDE_CONFIG_DIR")
            .map(|d| PathBuf::from(d).join(".claude.json"))
            .unwrap_or_else(|| dirs::home_dir().unwrap_or_default().join(".claude.json"));
        if let Ok(text) = std::fs::read_to_string(state) {
            h.add_state(&text);
        }
        h
    }

    fn add_state(&mut self, text: &str) {
        let Ok(v) = serde_json::from_str::<serde_json::Value>(text) else {
            return;
        };
        let Some(projects) = v["projects"].as_object() else {
            return;
        };
        for (dir, p) in projects {
            let Some(usage) = p["lastModelUsage"].as_object() else {
                continue;
            };
            for id in usage.keys() {
                if let Some(base) = id.to_lowercase().strip_suffix("[1m]") {
                    let base = base.to_string();
                    self.project_1m
                        .entry(dir.clone())
                        .or_default()
                        .push(base.clone());
                    if !self.any_1m.contains(&base) {
                        self.any_1m.push(base);
                    }
                }
            }
        }
    }

    /// Is `model` (as it appears in a transcript) known to run with 1M here?
    fn says_1m(&self, model: &str, cwd: Option<&str>) -> bool {
        let m = model.to_lowercase();
        if self
            .settings_1m
            .as_deref()
            .is_some_and(|fam| !fam.is_empty() && m.contains(fam))
        {
            return true;
        }
        let in_project = cwd
            .and_then(|c| self.project_1m.get(c))
            .is_some_and(|ids| ids.contains(&m));
        in_project || self.any_1m.contains(&m)
    }
}

static HINTS: LazyLock<Hints> = LazyLock::new(|| Hints::load(&super::discovery::claude_home()));

fn settings_1m_model(home: &Path) -> Option<String> {
    let text = std::fs::read_to_string(home.join("settings.json")).ok()?;
    let v: serde_json::Value = serde_json::from_str(&text).ok()?;
    let m = v["model"].as_str()?.to_lowercase();
    m.strip_suffix("[1m]").map(str::to_string)
}

/// Context usage for a live session, with the reasoning effort and the `timestamp` of the same
/// entry, cached per transcript `(size, mtime)`.
pub fn for_session(home: &Path, cwd: Option<&str>, session_id: &str) -> Option<LastReply> {
    let path = locate_transcript(home, cwd, session_id)?;
    for_transcript(&path, cwd, None).0
}

/// One cached tail supplies both the last reply and, when current sidecars need it
/// (`search` is `Some`), the current Turn's prompt and the subagents' lifecycle evidence.
/// Exact-target lookups do not call this enrichment. The last item is what the session is
/// doing now: `"{tool}: {summary}"` of the tool call still waiting for its result,
/// `Thinking`, or `None` (see [`Activity`]); a prompt with no reply yet is `Thinking`.
pub(crate) fn for_transcript(
    path: &Path,
    cwd: Option<&str>,
    search: Option<&Search>,
) -> (
    Option<LastReply>,
    Option<String>,
    super::subagents::Lifecycle,
    Option<String>,
) {
    let Some(meta) = std::fs::metadata(path).ok() else {
        return (None, None, Default::default(), None);
    };
    let key: CacheKey = (path.to_path_buf(), meta.len(), meta.modified().ok());
    let cached = CACHE.lock().ok().and_then(|c| {
        c.get(path)
            .filter(|(k, searched, raw)| {
                *k == key && (search.is_none() || *searched) && !raw.wants_wider(search)
            })
            .map(|(_, _, v)| v.clone())
    });
    let raw = cached.unwrap_or_else(|| {
        let raw = last_tail_in(path, search);
        if let Ok(mut c) = CACHE.lock() {
            c.insert(path.to_path_buf(), (key, search.is_some(), raw.clone()));
        }
        raw
    });
    let reply = raw.reply.map(|last| reply_with_window(last, cwd));
    (reply, raw.turn_start, raw.lifecycle, raw.activity)
}

fn reply_with_window(last: LastTurn, cwd: Option<&str>) -> LastReply {
    let LastTurn {
        used,
        model,
        effort,
        at,
    } = last;
    let window = window_for(model.as_deref(), used, cwd, &HINTS);
    LastReply {
        context: ContextUsage::new(used, window, model),
        effort,
        at,
    }
}

/// A model that some session has already pushed past 200k *without* a `[1m]`
/// suffix has a 1M window natively; lift its other sessions to 1M too, so one
/// still under 200k isn't read against the wrong window.
pub fn apply_native_1m<'a>(contexts: impl IntoIterator<Item = &'a mut ContextUsage>) {
    let mut all: Vec<&mut ContextUsage> = contexts.into_iter().collect();
    let native: Vec<String> = all
        .iter()
        .filter(|c| c.used > WINDOW_200K)
        .filter_map(|c| c.model.clone())
        .collect();
    for c in all.iter_mut() {
        if c.window < WINDOW_1M && c.model.as_ref().is_some_and(|m| native.contains(m)) {
            **c = ContextUsage::new(c.used, WINDOW_1M, c.model.take());
        }
    }
}

/// Uncached read of one transcript's tail, with the window inferred from the
/// model id and `hints`.
pub fn from_transcript(path: &Path, cwd: Option<&str>, hints: &Hints) -> Option<ContextUsage> {
    let LastTurn { used, model, .. } = last_usage_in(path)?;
    let window = window_for(model.as_deref(), used, cwd, hints);
    Some(ContextUsage::new(used, window, model))
}

/// The last usable assistant entry, reading only the tail.
fn last_usage_in(path: &Path) -> Option<LastTurn> {
    last_tail_in(path, None).reply
}

fn last_tail_in(path: &Path, search: Option<&Search>) -> Raw {
    read_tail(path, search).unwrap_or_default()
}

fn read_tail(path: &Path, search: Option<&Search>) -> Option<Raw> {
    let need_turn = search.is_some();
    let mut file = File::open(path).ok()?;
    let size = file.metadata().ok()?.len();
    let mut found = Raw::default();
    for tail in TAILS {
        let start = size.saturating_sub(tail);
        file.seek(SeekFrom::Start(start)).ok()?;
        let mut buf = Vec::with_capacity((size - start) as usize);
        file.by_ref()
            .take(size - start)
            .read_to_end(&mut buf)
            .ok()?;
        let text = String::from_utf8_lossy(&buf);
        let text = if start > 0 {
            // The first line is almost certainly cut in half.
            &text[text.find('\n').map_or(text.len(), |i| i + 1)..]
        } else {
            &text
        };
        found = scan_tail(text, need_turn);
        found.exhausted = start == 0 || tail == TAILS[TAILS.len() - 1];
        if need_turn {
            found.oldest = oldest_timestamp(text);
        }
        #[cfg(test)]
        tests::TAIL_READ.with(|read| read.set(size - start));
        if found.exhausted || (found.reply.is_some() && !found.wants_wider(search)) {
            break;
        }
    }
    Some(found)
}

/// One reverse pass over the same buffer for every fact, parsing each candidate once.
/// With `need_turn` it runs to the buffer's start: a subagent's latest notification can
/// predate the Turn's prompt.
fn scan_tail(text: &str, need_turn: bool) -> Raw {
    let mut found = Raw::default();
    let mut activity = Activity::default();
    for line in text.lines().rev() {
        let reply =
            found.reply.is_none() && line.contains("\"usage\"") && line.contains("\"assistant\"");
        let prompt = need_turn
            && !found.prompt_found
            && (line.contains("\"user\"") || line.contains("\"attachment\""));
        let lifecycle = need_turn && super::subagents::Lifecycle::candidate(line);
        let act = activity.wants(line);
        if !reply && !prompt && !lifecycle && !act {
            continue;
        }
        let Ok(v) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        if prompt && is_turn_prompt(&v) {
            found.prompt_found = true;
            found.turn_start = v["timestamp"]
                .as_str()
                .filter(|at| chrono::DateTime::parse_from_rfc3339(at).is_ok())
                .map(str::to_owned);
        }
        if reply {
            found.reply = usage_entry(&v);
        }
        if lifecycle {
            found.lifecycle.observe(&v);
        }
        if act {
            activity.observe(&v);
        }
        if found.reply.is_some() && !need_turn && activity.done.is_some() {
            break;
        }
    }
    found.activity = activity.finish();
    found
}

/// What a session is doing now, decided by walking its main-thread `assistant` and `user`
/// entries backwards (everything else is bookkeeping). The first entry met sets the base
/// answer: `Thinking` after a result, a thinking or text block mid-turn, or a prompt; none
/// after `end_turn` or an interrupt. Walking on, the latest `tool_use` whose id no later
/// `tool_result` answers wins (parallel calls interleave their results). A prompt or an
/// `end_turn` ends the walk, and so does an earlier API message once every result met is
/// answered: the model writes a message only after every call of the one before has its
/// result, so a pending call can only be in the latest. The walk so stays within the last
/// message or two, short of the buffer's edge in a long turn. A result whose call lies
/// before the buffer may hide a pending sibling, so that is none, not a guess.
#[derive(Default)]
struct Activity {
    /// The first meaningful entry has been seen.
    started: bool,
    /// `tool_result` ids met whose `tool_use` has not been.
    open: std::collections::HashSet<String>,
    /// The answer if no pending call turns up.
    base: Option<String>,
    /// The `message.id` met with `open` empty: an entry of any other message ends the walk.
    /// Parallel sibling calls are separate entries of one message, so they share it.
    settled: Option<String>,
    /// The walk is over, with this answer.
    done: Option<Option<String>>,
}

impl Activity {
    fn wants(&self, line: &str) -> bool {
        self.done.is_none() && (line.contains("\"assistant\"") || line.contains("\"user\""))
    }

    fn finish(self) -> Option<String> {
        match self.done {
            Some(done) => done,
            // The buffer ended before the walk did. Results still open after a settled
            // message answer an earlier one, whose calls all have their results.
            None if !self.open.is_empty() && self.settled.is_none() => None,
            None => self.base,
        }
    }

    /// A turn boundary (a prompt, an earlier `end_turn`): the walk ends here.
    fn boundary(&mut self) {
        self.done = Some(if self.open.is_empty() {
            self.base.take()
        } else {
            None
        });
    }

    /// An entry of the turn in progress; the first one met sets the base answer.
    fn mid_turn(&mut self) {
        if !self.started {
            self.base = Some("Thinking".to_string());
        }
        self.started = true;
    }

    fn observe(&mut self, v: &serde_json::Value) {
        if self.done.is_some()
            || v["isSidechain"] == true
            || v["isMeta"] == true
            || !matches!(v["type"].as_str(), Some("assistant" | "user"))
        {
            return;
        }
        let msg = &v["message"];
        if v["type"] == "assistant" {
            if v["isApiErrorMessage"] == true || msg["model"] == "<synthetic>" {
                return;
            }
            let id = msg["id"].as_str();
            if self.settled.as_deref().is_some_and(|s| id != Some(s)) {
                // An earlier message: its calls are answered, or the later one would not be
                // there. The results still open are its own.
                self.done = Some(self.base.take());
                return;
            }
            for b in msg["content"].as_array().into_iter().flatten().rev() {
                if b["type"] != "tool_use" {
                    continue;
                }
                if b["id"].as_str().is_some_and(|id| self.open.remove(id)) {
                    continue;
                }
                let name = b["name"].as_str().unwrap_or_default();
                if name.is_empty() || name == "AskUserQuestion" {
                    continue;
                }
                let summary = super::transcript::tool_summary(name, &b["input"]);
                let phrase = if summary.is_empty() {
                    name.to_string()
                } else {
                    format!("{name}: {summary}")
                };
                // One line for a terminal row: no carriage return, escape, tab or separator.
                let plain = |c: char| c.is_control() || matches!(c, '\u{2028}' | '\u{2029}');
                self.done = Some(Some(phrase.replace(plain, " ")));
                return;
            }
            if msg["stop_reason"] == "end_turn" {
                if self.started {
                    self.boundary();
                } else {
                    self.done = Some(None);
                }
            } else {
                self.mid_turn();
            }
            if self.settled.is_none() && self.open.is_empty() {
                self.settled = id.map(str::to_string);
            }
            return;
        }
        // A `user` entry: a result, an interrupt marker, or a prompt (a turn's start).
        let content = &msg["content"];
        let blocks = content.as_array();
        let results: Vec<&str> = blocks
            .into_iter()
            .flatten()
            .filter(|b| b["type"] == "tool_result")
            .filter_map(|b| b["tool_use_id"].as_str())
            .collect();
        if !results.is_empty() {
            self.open.extend(results.into_iter().map(str::to_string));
            self.mid_turn();
            return;
        }
        let interrupted = |t: &str| t.trim_start().starts_with("[Request interrupted");
        let marker = content.as_str().is_some_and(interrupted)
            || blocks.is_some_and(|b| b.iter().filter_map(|b| b["text"].as_str()).any(interrupted));
        if marker {
            // The session is idle after an interrupt; any call before it is stale.
            self.done = Some(None);
        } else if self.started {
            self.boundary();
        } else {
            // Your prompt, nothing since: the model is thinking.
            self.done = Some(Some("Thinking".to_string()));
        }
    }
}

/// The first entry's `timestamp` in a buffer of whole lines.
fn oldest_timestamp(text: &str) -> Option<SystemTime> {
    text.lines()
        .filter(|line| line.contains("\"timestamp\""))
        .find_map(|line| {
            let v: serde_json::Value = serde_json::from_str(line).ok()?;
            let at = chrono::DateTime::parse_from_rfc3339(v["timestamp"].as_str()?).ok()?;
            Some(SystemTime::from(at))
        })
}

fn is_turn_prompt(v: &serde_json::Value) -> bool {
    if v["isSidechain"] == true || v["isMeta"] == true || v["isCompactSummary"] == true {
        return false;
    }
    let (origin, content) = if v["type"] == "attachment" {
        let a = &v["attachment"];
        if a["type"] != "queued_command" || a["commandMode"] != "prompt" {
            return false;
        }
        (&a["origin"], &a["prompt"])
    } else if v["type"] == "user" {
        if v.get("toolUseResult").is_some() {
            return false;
        }
        (&v["origin"], &v["message"]["content"])
    } else {
        return false;
    };
    let human = origin["kind"] == "human";
    let sdk = v["type"] == "user" && origin.is_null() && v["turnOrigin"] == "sdk";
    if !human && !sdk {
        return false;
    }
    if let Some(blocks) = content.as_array()
        && blocks.iter().any(|b| b["type"] == "tool_result")
    {
        return false;
    }
    // Even a human-tagged interrupt or notification is not a new prompt.
    let is_noise = |text: &str| {
        let text = text.trim_start();
        text.starts_with("[Request interrupted by user")
            || text.starts_with("<task-notification>")
            || text.starts_with("<local-command-")
    };
    if content.as_str().is_some_and(is_noise) {
        return false;
    }
    if let Some(blocks) = content.as_array()
        && blocks
            .iter()
            .filter_map(|b| b["text"].as_str())
            .any(is_noise)
    {
        return false;
    }
    true
}

fn usage_entry(v: &serde_json::Value) -> Option<LastTurn> {
    if v["type"] != "assistant" || v["isSidechain"] == true || v["isApiErrorMessage"] == true {
        return None;
    }
    let msg = &v["message"];
    let model = msg["model"].as_str().map(str::to_string);
    if model.as_deref() == Some("<synthetic>") {
        return None;
    }
    let u = &msg["usage"];
    let n = |k: &str| u[k].as_u64().unwrap_or(0);
    let used = n("input_tokens") + n("cache_read_input_tokens") + n("cache_creation_input_tokens");
    if used == 0 {
        return None;
    }
    let effort = v["effort"]
        .as_str()
        .map(str::trim)
        .filter(|e| !e.is_empty())
        .map(str::to_string);
    let at = v["timestamp"].as_str().map(str::to_string);
    Some(LastTurn {
        used,
        model,
        effort,
        at,
    })
}

fn window_for(model: Option<&str>, used: u64, cwd: Option<&str>, hints: &Hints) -> u64 {
    let model = model.unwrap_or("").to_lowercase();
    if model.ends_with("[1m]")
        || native_1m(&model)
        || used > WINDOW_200K
        || hints.says_1m(&model, cwd)
    {
        WINDOW_1M
    } else {
        WINDOW_200K
    }
}

/// Models that run with 1M in Claude Code without a `[1m]` suffix: every Fable
/// and Mythos, and Opus / Sonnet from 5.5 on. Earlier Opus / Sonnet (4.6–5) are
/// 1M only when opted into via `[1m]`, which the hints cover.
fn native_1m(model: &str) -> bool {
    let Some(rest) = model.strip_prefix("claude-") else {
        return false;
    };
    let Some((family, version)) = rest.split_once('-') else {
        return false;
    };
    match family {
        "fable" | "mythos" => true,
        "opus" | "sonnet" => {
            let mut nums = version.split('-').map(|p| p.parse::<u32>().ok());
            let major = nums.next().flatten().unwrap_or(0);
            // A dated snapshot (`-20251101`) is not a minor version.
            let minor = nums.next().flatten().filter(|n| *n < 100).unwrap_or(0);
            (major, minor) >= (5, 5)
        }
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    thread_local! {
        /// Bytes of the last tail [`read_tail`] read, to prove how far it widened.
        pub(crate) static TAIL_READ: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
    }

    /// A search for the Turn's prompt, as a finished sidecar asks for.
    fn turn() -> Search {
        Search {
            awaited: Vec::new(),
            quiet: Vec::new(),
            turn: true,
        }
    }

    fn assistant(used_in: u64, cache_read: u64, cache_new: u64, model: &str, side: bool) -> String {
        serde_json::json!({
            "type": "assistant",
            "isSidechain": side,
            "message": {
                "role": "assistant",
                "model": model,
                "usage": {
                    "input_tokens": used_in,
                    "cache_read_input_tokens": cache_read,
                    "cache_creation_input_tokens": cache_new,
                    "output_tokens": 999
                }
            }
        })
        .to_string()
    }

    fn user(text: &str) -> String {
        serde_json::json!({"type": "user", "message": {"role": "user", "content": text}})
            .to_string()
    }

    fn write(dir: &Path, lines: &[String]) -> PathBuf {
        let p = dir.join("s.jsonl");
        let mut f = File::create(&p).unwrap();
        for l in lines {
            writeln!(f, "{l}").unwrap();
        }
        p
    }

    #[test]
    fn the_last_main_thread_turn_counts_and_output_does_not() {
        let d = tempfile::tempdir().unwrap();
        let p = write(
            d.path(),
            &[
                user("hi"),
                assistant(10, 1000, 0, "claude-sonnet-4-5", false),
                assistant(5, 100_000, 20_000, "claude-sonnet-4-5", false),
                // Subagent turns and synthetic/zero entries don't move the meter.
                assistant(1, 190_000, 0, "claude-haiku-4-5", true),
                assistant(0, 0, 0, "<synthetic>", false),
                assistant(0, 0, 0, "claude-sonnet-4-5", false),
                user("next"),
            ],
        );
        let c = from_transcript(&p, None, &Hints::default()).unwrap();
        assert_eq!(c.used, 120_005);
        assert_eq!(c.window, WINDOW_200K);
        assert_eq!(c.pct, 60);
        assert_eq!(c.model.as_deref(), Some("claude-sonnet-4-5"));
    }

    // `model_at`: the timestamp of the reply `model` was read from (testdata's synthetic
    // transcript: a reply, then `/model` entries, which are no reply).
    #[test]
    fn the_model_comes_with_the_time_of_its_reply() {
        let p = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../testdata/claude/model-transcript.jsonl");
        let last = last_usage_in(&p).unwrap();
        assert_eq!(last.model.as_deref(), Some("claude-opus-5-5"));
        assert_eq!(last.at.as_deref(), Some("2026-01-02T03:04:30.000Z"));
        // An entry without one: no time.
        let d = tempfile::tempdir().unwrap();
        let p = write(d.path(), &[assistant(1, 10, 0, "claude-opus-5-5", false)]);
        assert_eq!(last_usage_in(&p).unwrap().at, None);
    }

    #[test]
    fn the_window_is_inferred() {
        let none = Hints::default();
        let w =
            |m: &str, used: u64, cwd: Option<&str>, h: &Hints| window_for(Some(m), used, cwd, h);
        assert_eq!(w("claude-opus-4-5", 150_000, None, &none), WINDOW_200K);
        assert_eq!(w("claude-sonnet-4-5[1m]", 10, None, &none), WINDOW_1M);
        // Natively 1M without a suffix: Fable, Mythos, Opus / Sonnet 5.5+.
        for m in [
            "claude-opus-5-5",
            "claude-sonnet-5-5",
            "claude-opus-6",
            "claude-fable-5-1",
            "claude-mythos-5",
        ] {
            assert_eq!(w(m, 10, None, &none), WINDOW_1M, "{m}");
        }
        for m in [
            "claude-opus-5",
            "claude-haiku-4-5-20251001",
            "claude-opus-4-5-20251101",
        ] {
            assert_eq!(w(m, 10, None, &none), WINDOW_200K, "{m}");
        }
        // Can't be 200k if the prompt is already bigger than that.
        assert_eq!(w("claude-opus-4-5", 250_000, None, &none), WINDOW_1M);
        // Settings pin `sonnet[1m]`: sonnet sessions get 1M, others don't.
        let pinned = Hints {
            settings_1m: Some("sonnet".into()),
            ..Default::default()
        };
        assert_eq!(w("claude-sonnet-4-5", 10, None, &pinned), WINDOW_1M);
        assert_eq!(w("claude-haiku-4-5", 10, None, &pinned), WINDOW_200K);
        // Claude Code's own state recorded `claude-opus-4-8[1m]`.
        let mut state = Hints::default();
        state.add_state(
            r#"{"projects":{"/work/app":{"lastModelUsage":{"claude-opus-4-8[1m]":{},"claude-haiku-4-5":{}}}}}"#,
        );
        assert_eq!(
            state.project_1m["/work/app"],
            vec!["claude-opus-4-8".to_string()]
        );
        assert_eq!(
            w("claude-opus-4-8", 10, Some("/work/app"), &state),
            WINDOW_1M
        );
        assert_eq!(w("claude-opus-4-8", 10, Some("/other"), &state), WINDOW_1M);
        assert_eq!(
            w("claude-haiku-4-5", 10, Some("/work/app"), &state),
            WINDOW_200K
        );
        // Garbage state is no hint at all.
        state.add_state("not json");
    }

    #[test]
    fn a_model_seen_past_200k_is_natively_1m_everywhere() {
        let mut rows = [
            ContextUsage::new(300_000, WINDOW_1M, Some("claude-x".into())),
            ContextUsage::new(100_000, WINDOW_200K, Some("claude-x".into())),
            ContextUsage::new(100_000, WINDOW_200K, Some("claude-y".into())),
        ];
        apply_native_1m(rows.iter_mut());
        assert_eq!((rows[1].window, rows[1].pct), (WINDOW_1M, 10));
        assert_eq!(rows[1].model.as_deref(), Some("claude-x"));
        assert_eq!((rows[2].window, rows[2].pct), (WINDOW_200K, 50));
    }

    #[test]
    fn settings_hint_reads_a_1m_model() {
        let d = tempfile::tempdir().unwrap();
        std::fs::write(d.path().join("settings.json"), r#"{"model":"Sonnet[1m]"}"#).unwrap();
        assert_eq!(settings_1m_model(d.path()).as_deref(), Some("sonnet"));
        std::fs::write(d.path().join("settings.json"), r#"{"model":"opus"}"#).unwrap();
        assert_eq!(settings_1m_model(d.path()), None);
    }

    #[test]
    fn a_big_transcript_is_read_from_the_tail_and_widened_when_needed() {
        let d = tempfile::tempdir().unwrap();
        let filler = user(&"x".repeat(1000));
        let mut lines = vec![assistant(1, 50_000, 0, "claude-opus-4-5", false)];
        // ~600 KiB of user lines after the only assistant entry: the first 256 KiB
        // tail misses it, the 2 MiB one finds it.
        lines.extend(std::iter::repeat_n(filler, 600));
        let p = write(d.path(), &lines);
        let c = from_transcript(&p, None, &Hints::default()).unwrap();
        assert_eq!(c.used, 50_001);
        assert_eq!(c.pct, 25);
    }

    #[test]
    fn no_usage_is_none() {
        let d = tempfile::tempdir().unwrap();
        let p = write(d.path(), &[user("hi"), "not json".into()]);
        assert_eq!(from_transcript(&p, None, &Hints::default()), None);
        assert_eq!(
            from_transcript(&d.path().join("missing.jsonl"), None, &Hints::default()),
            None
        );
    }

    #[test]
    fn transcripts_are_found_by_cwd_or_by_scanning() {
        let home = tempfile::tempdir().unwrap();
        let proj = home.path().join("projects");
        std::fs::create_dir_all(proj.join("-work-app")).unwrap();
        std::fs::create_dir_all(proj.join("-elsewhere")).unwrap();
        std::fs::write(proj.join("-work-app/aaa.jsonl"), "").unwrap();
        std::fs::write(proj.join("-elsewhere/bbb.jsonl"), "").unwrap();
        assert_eq!(
            locate_transcript(home.path(), Some("/work/app"), "aaa"),
            Some(proj.join("-work-app/aaa.jsonl"))
        );
        // cwd changed since launch: still found.
        assert_eq!(
            locate_transcript(home.path(), Some("/work/app"), "bbb"),
            Some(proj.join("-elsewhere/bbb.jsonl"))
        );
        assert_eq!(locate_transcript(home.path(), None, "ccc"), None);
    }

    fn effort_of(fixture: &str) -> Option<String> {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("s.jsonl");
        std::fs::write(&p, fixture).unwrap();
        last_usage_in(&p).unwrap().effort
    }

    #[test]
    fn effort_is_the_last_main_thread_turns() {
        // A live transcript, trimmed: medium, then `/effort high`, then a reply at high; a
        // subagent turn at low and a synthetic entry follow.
        let change = include_str!("../../../../testdata/claude/effort-change.jsonl");
        assert_eq!(effort_of(change).as_deref(), Some("high"));
        // Right after `/effort high`, before a reply: still the last reply's.
        let before: Vec<&str> = change.lines().take(5).collect();
        assert_eq!(effort_of(&before.join("\n")).as_deref(), Some("medium"));
        // SDK sessions write `perTurnEffort: null`; `effort` still says it.
        let sdk = include_str!("../../../../testdata/claude/effort-sdk.jsonl");
        assert_eq!(effort_of(sdk).as_deref(), Some("high"));
        // A Claude Code that records no effort.
        let none = include_str!("../../../../testdata/claude/effort-none.jsonl");
        assert_eq!(effort_of(none), None);
    }

    #[test]
    fn for_session_gives_the_context_and_the_effort() {
        let home = tempfile::tempdir().unwrap();
        let dir = home.path().join("projects/-work-app");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("ccc.jsonl"),
            include_str!("../../../../testdata/claude/effort-change.jsonl"),
        )
        .unwrap();
        let LastReply {
            context: c, effort, ..
        } = for_session(home.path(), Some("/work/app"), "ccc").unwrap();
        assert_eq!(c.used, 41_603);
        assert_eq!(c.model.as_deref(), Some("claude-opus-5-5"));
        assert_eq!(effort.as_deref(), Some("high"));
    }

    fn fixture(name: &str) -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../testdata/claude")
            .join(format!("activity-{name}.jsonl"))
    }

    fn activity_of(name: &str) -> Option<String> {
        last_tail_in(&fixture(name), None).activity
    }

    #[test]
    fn a_pending_call_is_what_the_session_is_doing() {
        for (name, expected) in [
            ("pending-read", "Read: /work/app/src/main.rs"),
            ("pending-edit", "Edit: /work/app/web/src/App.tsx"),
            // The first line of the command, as the chat shows it, not the description.
            ("pending-bash", "Bash: bundle exec rspec spec/models"),
            ("pending-grep", "Grep: needle"),
        ] {
            assert_eq!(activity_of(name).as_deref(), Some(expected), "{name}");
        }
    }

    #[test]
    fn parallel_calls_show_the_latest_one_without_a_result() {
        // `a` is answered; `b` was issued after it and is still running.
        assert_eq!(
            activity_of("parallel-one-answered").as_deref(),
            Some("Grep: needle")
        );
        // Results interleave with later siblings: `a` and `b` are answered, `c` is not.
        assert_eq!(
            activity_of("parallel-interleaved").as_deref(),
            Some("Bash: ls -la")
        );
    }

    #[test]
    fn no_pending_call_is_thinking_until_the_turn_ends() {
        assert_eq!(activity_of("all-answered").as_deref(), Some("Thinking"));
        // The skipped question leaves the turn thinking: Fleet's `waiting_for` says the rest.
        assert_eq!(activity_of("ask-skipped").as_deref(), Some("Thinking"));
        assert_eq!(activity_of("end-turn"), None);
        assert_eq!(activity_of("interrupt"), None);
    }

    #[test]
    fn the_scan_goes_past_the_usage_entry_the_reply_stops_at() {
        // The newest usage entry is the `Bash` call, but the walk still reads on to decide.
        let raw = last_tail_in(&fixture("all-answered"), None);
        assert!(raw.reply.is_some());
        assert_eq!(raw.activity.as_deref(), Some("Thinking"));
        // A prompt and nothing else; and nothing at all.
        let d = tempfile::tempdir().unwrap();
        let p = write(d.path(), &[user("hi")]);
        assert_eq!(last_tail_in(&p, None).activity.as_deref(), Some("Thinking"));
        assert_eq!(last_tail_in(&write(d.path(), &[]), None).activity, None);
    }

    #[test]
    fn a_result_whose_call_is_beyond_the_buffer_is_no_answer() {
        let result = serde_json::json!({"type": "user", "message": {"role": "user", "content": [
            {"type": "tool_result", "tool_use_id": "gone", "content": "ok"}]}})
        .to_string();
        let d = tempfile::tempdir().unwrap();
        let p = write(
            d.path(),
            &[assistant(1, 10, 0, "claude-opus-5-5", false), result],
        );
        // A sibling of `gone` may still be pending out there: not `Thinking`.
        assert_eq!(last_tail_in(&p, None).activity, None);
    }

    /// An entry of API message `id` holding one content block, as Claude Code writes them.
    fn block(id: &str, block: serde_json::Value) -> String {
        serde_json::json!({"type": "assistant", "isSidechain": false, "message": {
            "id": id, "model": "claude-opus-5-5", "role": "assistant", "content": [block],
            "stop_reason": "tool_use", "usage": {"input_tokens": 3,
            "cache_read_input_tokens": 20000, "cache_creation_input_tokens": 800,
            "output_tokens": 50}}})
        .to_string()
    }

    fn call(id: &str, use_id: &str, name: &str, input: serde_json::Value) -> String {
        block(
            id,
            serde_json::json!({"type": "tool_use", "id": use_id, "name": name, "input": input}),
        )
    }

    fn result(use_id: &str, content: &str) -> String {
        serde_json::json!({"type": "user", "isSidechain": false, "message": {"role": "user",
            "content": [{"type": "tool_result", "tool_use_id": use_id, "content": content}]}})
        .to_string()
    }

    #[test]
    fn a_long_turn_is_thinking_though_the_buffer_splits_an_earlier_call_from_its_result() {
        // `z`'s result alone outgrows the first tail, so the buffer holds `a`'s result but
        // not the call: the walk must stop at `msg_2`, the message before the latest.
        let lines = [
            user("fix the rates"),
            call(
                "msg_1",
                "a",
                "Read",
                serde_json::json!({"file_path": "/work/app/a.rs"}),
            ),
            call(
                "msg_1",
                "z",
                "Bash",
                serde_json::json!({"command": "cat build.log"}),
            ),
            result("z", &"x".repeat(300 * 1024)),
            result("a", "ok"),
            call(
                "msg_2",
                "b",
                "Grep",
                serde_json::json!({"pattern": "needle"}),
            ),
            result("b", "ok"),
            call(
                "msg_3",
                "c",
                "Read",
                serde_json::json!({"file_path": "/work/app/c.rs"}),
            ),
            result("c", "ok"),
            block(
                "msg_4",
                serde_json::json!({"type": "thinking", "thinking": ""}),
            ),
        ];
        // Without `msg_2`, the buffer ends after `a`'s result, before any `msg_1` entry.
        let unstopped: Vec<_> = [&lines[..5], &lines[7..]].concat();
        let d = tempfile::tempdir().unwrap();
        // Thinking after the latest result, and after a thinking block.
        for lines in [&lines[..], &unstopped[..]] {
            for n in [lines.len() - 1, lines.len()] {
                let p = write(d.path(), &lines[..n]);
                TAIL_READ.with(|read| read.set(0));
                let raw = last_tail_in(&p, None);
                assert_eq!(raw.activity.as_deref(), Some("Thinking"), "{n} lines");
                assert_eq!(TAIL_READ.with(|read| read.get()), TAILS[0], "not widened");
            }
        }
    }

    #[test]
    fn a_prompt_with_no_reply_yet_is_thinking() {
        let d = tempfile::tempdir().unwrap();
        let p = write(d.path(), &[user("fix the rates")]);
        let (reply, _, _, activity) = for_transcript(&p, None, None);
        assert!(reply.is_none());
        assert_eq!(activity.as_deref(), Some("Thinking"));
    }

    #[test]
    fn the_activity_is_one_plain_line() {
        let command = "echo a\rb\t\u{1b}[1mc\u{2028}d\u{2029}e\nsecond line";
        let d = tempfile::tempdir().unwrap();
        let p = write(
            d.path(),
            &[
                user("fix the rates"),
                call(
                    "msg_1",
                    "a",
                    "Bash",
                    serde_json::json!({"command": command}),
                ),
            ],
        );
        assert_eq!(
            last_tail_in(&p, None).activity.as_deref(),
            Some("Bash: echo a b  [1mc d e")
        );
    }

    #[test]
    fn the_activity_says_what_the_chat_says() {
        use super::super::{harness::Harness, transcript};
        let bytes = std::fs::read(fixture("pending-bash")).unwrap();
        let parsed = transcript::parse(Harness::Claude, &bytes, 0);
        let chat = parsed.entries.iter().rfind(|e| e.tool.is_some()).unwrap();
        assert_eq!(
            activity_of("pending-bash").as_deref(),
            Some(chat.text.as_str())
        );
    }

    #[test]
    fn levels_band_the_percentage() {
        assert_eq!(level(59), Level::Low);
        assert_eq!(level(60), Level::Warn);
        assert_eq!(level(85), Level::Warn);
        assert_eq!(level(86), Level::Hot);
    }
    #[test]
    fn current_turn_fixtures_distinguish_prompts_from_bookkeeping() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../testdata/subagents");
        for (file, expected) in [
            ("typed.jsonl", Some("2026-01-01T00:01:00Z")),
            ("mid-turn.jsonl", Some("2026-01-01T00:01:05Z")),
            ("followed-by-noise.jsonl", Some("2026-01-01T00:01:00Z")),
            ("sdk.jsonl", Some("2026-01-01T00:01:00Z")),
            ("skill.jsonl", Some("2026-01-01T00:01:00Z")),
            ("no-prompt.jsonl", None),
        ] {
            let text = std::fs::read_to_string(root.join(file)).unwrap();
            assert_eq!(
                scan_tail(&text, true).turn_start.as_deref(),
                expected,
                "{file}"
            );
        }
    }

    #[test]
    fn turn_search_widens_even_with_a_recent_reply_and_stops_at_eight_mib() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("wide.jsonl");
        let prompt = include_str!("../../../../testdata/subagents/typed.jsonl")
            .lines()
            .next()
            .unwrap();
        let reply = assistant(10, 0, 0, "claude-sonnet-4-5", false);
        // Same widening steps as usage; the final case leaves the prompt beyond the cap.
        for padding in [300_000, 3_000_000, 8 * 1024 * 1024 + 10] {
            let mut file = File::create(&path).unwrap();
            writeln!(file, "{prompt}").unwrap();
            file.write_all(&vec![b' '; padding]).unwrap();
            writeln!(file, "\n{reply}").unwrap();
            let found = last_tail_in(&path, Some(&turn()));
            assert_eq!(found.reply.unwrap().used, 10);
            assert_eq!(found.turn_start.is_some(), padding < 8 * 1024 * 1024);
        }
    }

    #[test]
    fn tail_cache_adds_the_turn_search_on_demand_and_tracks_new_prompts() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("cached.jsonl");
        std::fs::write(
            &path,
            include_str!("../../../../testdata/subagents/typed.jsonl"),
        )
        .unwrap();
        assert!(for_transcript(&path, None, None).1.is_none());
        assert_eq!(
            for_transcript(&path, None, Some(&turn())).1.as_deref(),
            Some("2026-01-01T00:01:00Z")
        );
        let mut file = std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap();
        writeln!(
            file,
            "{}",
            serde_json::json!({"type":"user", "timestamp":"2026-01-01T00:03:00Z",
            "origin":{"kind":"human"}, "message":{"content":"Synthetic next prompt"}})
        )
        .unwrap();
        let (last, turn, ..) = for_transcript(&path, None, Some(&turn()));
        assert_eq!(turn.as_deref(), Some("2026-01-01T00:03:00Z"));
        assert_eq!(last.unwrap().effort.as_deref(), Some("high"));
    }

    #[test]
    fn an_undated_latest_prompt_never_reuses_an_earlier_turn() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("undated.jsonl");
        let agents = dir.path().join("undated/subagents");
        std::fs::create_dir_all(&agents).unwrap();
        std::fs::write(
            agents.join("agent-finished.meta.json"),
            r#"{"finished":true}"#,
        )
        .unwrap();
        std::fs::write(
            agents.join("agent-finished.jsonl"),
            "{\"timestamp\":\"2026-01-01T00:02:00Z\"}\n",
        )
        .unwrap();
        let old = include_str!("../../../../testdata/subagents/typed.jsonl")
            .lines()
            .next()
            .unwrap();
        for timestamp in [
            None,
            Some(serde_json::json!(123)),
            Some(serde_json::json!("invalid")),
        ] {
            for attachment in [false, true] {
                // A recent undated prompt, or one only reached after widening. The reply
                // may itself require widening, so an earlier timestamp must never win.
                for (before, after, reply_last) in [
                    (0, 0, true),
                    (300_000, 300_000, true),
                    (3_000_000, 3_000_000, true),
                    (300_000, 0, false),
                ] {
                    let mut prompt = if attachment {
                        serde_json::json!({"type":"attachment", "attachment":{"type":"queued_command",
                            "commandMode":"prompt", "origin":{"kind":"human"}, "prompt":"Synthetic next prompt"}})
                    } else {
                        serde_json::json!({"type":"user", "origin":{"kind":"human"},
                            "message":{"content":"Synthetic next prompt"}})
                    };
                    if let Some(ts) = &timestamp {
                        prompt["timestamp"] = ts.clone();
                    }
                    let reply = assistant(10, 0, 0, "claude-sonnet-4-5", false);
                    let mut file = File::create(&path).unwrap();
                    writeln!(file, "{old}").unwrap();
                    if !reply_last {
                        writeln!(file, "{reply}").unwrap();
                    }
                    file.write_all(&vec![b' '; before]).unwrap();
                    writeln!(file, "\n{prompt}").unwrap();
                    file.write_all(&vec![b' '; after]).unwrap();
                    if reply_last {
                        writeln!(file, "\n{reply}").unwrap();
                    }
                    let found = last_tail_in(&path, Some(&turn()));
                    assert_eq!(found.reply.unwrap().used, 10);
                    assert!(found.prompt_found);
                    assert!(
                        found.turn_start.is_none(),
                        "timestamp={timestamp:?}, attachment={attachment}, before={before}, after={after}"
                    );
                    let counts = crate::core::subagents::scan(
                        &path,
                        Some("2.1.301"),
                        Some(1_767_225_600_000),
                    )
                    .unwrap()
                    .counts(found.turn_start.as_deref(), &found.lifecycle);
                    assert_eq!(counts.finished, None);
                }
            }
        }
    }

    /// A synthetic Session: a parked subagent's interim note, `padding` bytes of later
    /// entries, an optional notice within the last 256 KiB, then the Turn's prompt and reply.
    /// `metas` are `(id, owner, end in seconds after 00:00)`.
    fn parked_session(
        padding: usize,
        recent: Option<&str>,
        metas: &[(&str, Option<&str>, u64)],
    ) -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("session.jsonl");
        let notice = |at: &str, id: &str, note: &str| {
            serde_json::json!({"type": "user", "timestamp": at,
                "origin": {"kind": "task-notification"},
                "message": {"content": format!("<task-notification>\n<task-id>{id}</task-id>\n\
                    <status>completed</status>\n<summary>Synthetic</summary>\n\
                    <note>Synthetic: {note}.</note>\n<result>Synthetic</result>\n\
                    </task-notification>")}})
        };
        let mut file = File::create(&path).unwrap();
        let interim = "background work of its own still running";
        writeln!(
            file,
            "{}",
            notice("2026-01-01T00:01:20Z", "parked", interim)
        )
        .unwrap();
        let line = serde_json::json!({"type": "progress", "timestamp": "2026-01-01T00:01:30Z",
            "data": "x".repeat(1000)})
        .to_string();
        for _ in 0..padding.div_ceil(line.len() + 1) {
            writeln!(file, "{line}").unwrap();
        }
        if let Some(note) = recent {
            writeln!(file, "{}", notice("2026-01-01T00:01:38Z", "parked", note)).unwrap();
        }
        writeln!(
            file,
            "{}",
            serde_json::json!({"type": "user", "timestamp": "2026-01-01T00:01:40Z",
                "origin": {"kind": "human"}, "message": {"content": "Synthetic prompt"}})
        )
        .unwrap();
        writeln!(file, "{}", assistant(10, 0, 0, "claude-sonnet-4-5", false)).unwrap();
        let agents = dir.path().join("session/subagents");
        std::fs::create_dir_all(&agents).unwrap();
        for (id, owner, end) in metas {
            let meta = agents.join(format!("agent-{id}.meta.json"));
            let mut v = serde_json::json!({"finished": true});
            if let Some(owner) = owner {
                v["parentAgentId"] = serde_json::json!(owner);
            }
            std::fs::write(&meta, v.to_string()).unwrap();
            let at = std::time::UNIX_EPOCH + std::time::Duration::from_secs(1_767_225_600 + end);
            File::options()
                .write(true)
                .open(&meta)
                .unwrap()
                .set_times(std::fs::FileTimes::new().set_modified(at))
                .unwrap();
        }
        (dir, path)
    }

    /// `list`'s path for one Session: the running count and how many bytes the tail read
    /// (0 when the cached tail served it).
    fn poll_parked(path: &Path) -> (u64, u64) {
        let s =
            crate::core::subagents::scan(path, Some("2.1.308"), Some(1_767_225_600_000)).unwrap();
        let search = s.search();
        TAIL_READ.with(|read| read.set(0));
        let (_, turn, lifecycle, _) = for_transcript(path, None, search.as_ref());
        let counts = s.counts(turn.as_deref(), &lifecycle);
        (counts.running, TAIL_READ.with(|read| read.get()))
    }

    #[test]
    fn an_interim_note_beyond_the_first_tail_widens_only_while_evidence_is_missing() {
        const FIRST: u64 = 256 * 1024;
        let size = |path: &Path| std::fs::metadata(path).unwrap().len();
        // The parked subagent ended at 00:01:20, before the 256 KiB tail's oldest entry,
        // with no evidence in it: widen until its interim note is in the buffer.
        let (_dir, path) = parked_session(300_000, None, &[("parked", None, 80)]);
        assert_eq!(poll_parked(&path), (1, size(&path)));
        // The widened tail is cached for the same sidecars.
        assert_eq!(poll_parked(&path), (1, 0));
        // Evidence in the first tail (a later settled note): no extra widening.
        let settled = "stops with no live background children";
        let (_dir, path) = parked_session(300_000, Some(settled), &[("parked", None, 80)]);
        assert_eq!(poll_parked(&path), (0, FIRST));
        // A sidecar that ended within the first tail has no notice yet: no widening.
        let (_dir, path) = parked_session(300_000, None, &[("unnoticed", None, 105)]);
        assert_eq!(poll_parked(&path), (0, FIRST));
        // Once the parked subagent's sidecar appears, the cached 256 KiB tail no longer
        // suffices and the next poll widens.
        let agents = path.with_extension("").join("subagents");
        std::fs::write(
            agents.join("agent-parked.meta.json"),
            r#"{"finished":true}"#,
        )
        .unwrap();
        let at = std::time::UNIX_EPOCH + std::time::Duration::from_secs(1_767_225_600 + 80);
        File::options()
            .write(true)
            .open(agents.join("agent-parked.meta.json"))
            .unwrap()
            .set_times(std::fs::FileTimes::new().set_modified(at))
            .unwrap();
        assert_eq!(poll_parked(&path), (1, size(&path)));
        // A nested sidecar's notices go to its owner: the parent's tail never widens for it.
        let (_dir, path) = parked_session(300_000, None, &[("parked", Some("owner"), 80)]);
        assert_eq!(poll_parked(&path), (0, FIRST));
        // Beyond 8 MiB, no notice means finished, as before.
        let (_dir, path) = parked_session(9 * 1024 * 1024, None, &[("parked", None, 80)]);
        assert_eq!(poll_parked(&path), (0, 8 * 1024 * 1024));
    }

    /// A synthetic Session: a successful `TaskStop` of `stopped` at 00:01:20, `padding` bytes
    /// of later entries from 00:01:30, then the Turn's prompt and reply. `stopped`'s meta
    /// lacks `finished` and was written at its launch, 00:01:10; its log was last written
    /// `log` seconds after 00:00, or is absent. `owner` makes it a nested one.
    fn stopped_session(
        padding: usize,
        log: Option<u64>,
        owner: Option<&str>,
    ) -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("session.jsonl");
        let mut file = File::create(&path).unwrap();
        let result = r#"{"message":"Successfully stopped task: stopped (Synthetic task)","task_id":"stopped","task_type":"local_agent","command":"Synthetic task"}"#;
        writeln!(
            file,
            "{}",
            serde_json::json!({"type": "user", "timestamp": "2026-01-01T00:01:20Z",
                "message": {"content": [{"type": "tool_result", "content": result}]}})
        )
        .unwrap();
        let line = serde_json::json!({"type": "progress", "timestamp": "2026-01-01T00:01:30Z",
            "data": "x".repeat(1000)})
        .to_string();
        for _ in 0..padding.div_ceil(line.len() + 1) {
            writeln!(file, "{line}").unwrap();
        }
        writeln!(
            file,
            "{}",
            serde_json::json!({"type": "user", "timestamp": "2026-01-01T00:01:40Z",
                "origin": {"kind": "human"}, "message": {"content": "Synthetic prompt"}})
        )
        .unwrap();
        writeln!(file, "{}", assistant(10, 0, 0, "claude-sonnet-4-5", false)).unwrap();
        let agents = dir.path().join("session/subagents");
        std::fs::create_dir_all(&agents).unwrap();
        let at = |seconds: u64| {
            std::fs::FileTimes::new().set_modified(
                std::time::UNIX_EPOCH + std::time::Duration::from_secs(1_767_225_600 + seconds),
            )
        };
        let meta = agents.join("agent-stopped.meta.json");
        let v = match owner {
            Some(owner) => serde_json::json!({"parentAgentId": owner}),
            None => serde_json::json!({}),
        };
        std::fs::write(&meta, v.to_string()).unwrap();
        File::options()
            .write(true)
            .open(&meta)
            .unwrap()
            .set_times(at(70))
            .unwrap();
        if let Some(seconds) = log {
            let log = agents.join("agent-stopped.jsonl");
            std::fs::write(&log, "{\"timestamp\":\"2026-01-01T00:01:10Z\"}\n").unwrap();
            File::options()
                .write(true)
                .open(&log)
                .unwrap()
                .set_times(at(seconds))
                .unwrap();
        }
        (dir, path)
    }

    #[test]
    fn a_stop_beyond_the_first_tail_widens_while_its_subagent_is_quiet() {
        const FIRST: u64 = 256 * 1024;
        let size = |path: &Path| std::fs::metadata(path).unwrap().len();
        // Its log was last written at 00:01:15, before the 256 KiB tail's oldest entry
        // (00:01:30), and the tail holds no evidence: widen until the stop is in the buffer.
        let (_dir, path) = stopped_session(300_000, Some(75), None);
        assert_eq!(poll_parked(&path), (0, size(&path)));
        // The widened tail is cached for the same sidecars.
        assert_eq!(poll_parked(&path), (0, 0));
        // A log written to since the tail's oldest entry: any stop is inside the tail.
        let (_dir, path) = stopped_session(300_000, Some(95), None);
        assert_eq!(poll_parked(&path), (1, FIRST));
        // No log to stat: no widening.
        let (_dir, path) = stopped_session(300_000, None, None);
        assert_eq!(poll_parked(&path), (1, FIRST));
        // A nested one's stop goes to its owner: the parent's tail never widens for it.
        let (_dir, path) = stopped_session(300_000, Some(75), Some("owner"));
        assert_eq!(poll_parked(&path), (1, FIRST));
        // Beyond 8 MiB it counts as running, as before.
        let (_dir, path) = stopped_session(9 * 1024 * 1024, Some(75), None);
        assert_eq!(poll_parked(&path), (1, 8 * 1024 * 1024));
    }

    #[test]
    fn unfinished_sidecars_search_only_the_tail_read_anyway() {
        const FIRST: u64 = 256 * 1024;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("session.jsonl");
        let mut file = File::create(&path).unwrap();
        writeln!(
            file,
            "{}",
            serde_json::json!({"type": "user", "timestamp": "2026-01-01T00:01:00Z",
                "origin": {"kind": "human"}, "message": {"content": "Synthetic prompt"}})
        )
        .unwrap();
        let line = serde_json::json!({"type": "progress", "timestamp": "2026-01-01T00:01:30Z",
            "data": "x".repeat(1000)})
        .to_string();
        for _ in 0..300 {
            writeln!(file, "{line}").unwrap();
        }
        // A successful TaskStop near the end, its result only a JSON string.
        let result = r#"{"message":"Successfully stopped task: stopped (Synthetic task)","task_id":"stopped","task_type":"local_agent","command":"Synthetic task"}"#;
        writeln!(
            file,
            "{}",
            serde_json::json!({"type": "user", "timestamp": "2026-01-01T00:01:38Z",
                "message": {"content": [{"type": "tool_result", "content": result}]}})
        )
        .unwrap();
        writeln!(file, "{}", assistant(10, 0, 0, "claude-sonnet-4-5", false)).unwrap();
        let agents = dir.path().join("session/subagents");
        std::fs::create_dir_all(&agents).unwrap();
        let sidecar = |id: &str, meta: &str, end: u64| {
            let path = agents.join(format!("agent-{id}.meta.json"));
            std::fs::write(&path, meta).unwrap();
            let at = std::time::UNIX_EPOCH + std::time::Duration::from_secs(1_767_225_600 + end);
            File::options()
                .write(true)
                .open(&path)
                .unwrap()
                .set_times(std::fs::FileTimes::new().set_modified(at))
                .unwrap();
            std::fs::write(
                agents.join(format!("agent-{id}.jsonl")),
                "{\"timestamp\":\"2026-01-01T00:01:10Z\"}\n",
            )
            .unwrap();
        };
        sidecar("stopped", "{}", 70);
        sidecar("running", "{}", 70);
        let poll = || {
            let s = crate::core::subagents::scan(&path, Some("2.1.308"), Some(1_767_225_600_000))
                .unwrap();
            let search = s.search();
            TAIL_READ.with(|read| read.set(0));
            let (_, turn, lifecycle, _) = for_transcript(&path, None, search.as_ref());
            let counts = s.counts(turn.as_deref(), &lifecycle);
            (
                counts.running,
                counts.finished,
                TAIL_READ.with(|read| read.get()),
            )
        };
        // The stop in the first tail settles its sidecar; the prompt beyond it is not
        // searched for, so the Turn, and whether it ended in it, stays unknown.
        assert_eq!(poll(), (1, None, FIRST));
        // A finished sidecar that ended inside that tail widens it only for the prompt.
        sidecar("done", r#"{"finished":true}"#, 99);
        let size = std::fs::metadata(&path).unwrap().len();
        assert_eq!(poll(), (1, Some(2), size));
    }
}
