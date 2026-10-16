//! A session's conversation, read from its transcript: what `fleet messages` prints.
//!
//! A port of the web app's `web/lib/transcript.mjs` (Claude Code, Codex and pi parsers), plus
//! tool calls, question outcomes and byte-position cursors:
//!
//! - Every **entry** comes from one transcript line — the line that started it — and its
//!   position is that line's byte offset plus an index among the entries the line produced.
//!   A position is stable for as long as the transcript is only appended to.
//! - A **cursor** is a position plus a fingerprint of its line (so a rewritten transcript is
//!   noticed). The cursor a page hands back for polling also carries a *read hint*: where the
//!   bytes it read ended, and a line from which parsing can restart with nothing lost. A
//!   `--after` call then reads from that line on — usually the end of what the last call read,
//!   so only new bytes.
//! - A restart line must be **clean**: no question waiting for its result, no queued prompt,
//!   no assistant entry that a later line could still extend (streamed blocks glued into one
//!   message; Codex narration a later tool call turns into non-final). Parsing from a clean line
//!   with fresh state yields exactly the entries a parse from further back would.
//! - Only complete lines are read: a half-written last line waits for the next call.

use std::borrow::Cow;
use std::collections::{HashMap, VecDeque};
use std::fs::File;
use std::io::{self, BufRead, BufReader, Read, Seek, SeekFrom};
use std::path::Path;
use std::rc::Rc;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::core::harness::Harness;
use crate::core::questions::{self, QUESTION_TOOL, Question, truthy};

/// The first tail read when no cursor is given; it doubles until it holds enough entries.
pub const TAIL_BYTES: u64 = 2 << 20;
/// Where the no-cursor tail stops growing (screenshots and tool output bloat transcripts).
pub const MAX_GROWN_TAIL_BYTES: u64 = 64 << 20;
/// Where a `--before` window stops growing.
pub const MAX_BEFORE_BYTES: u64 = 256 << 20;
/// The most an `--after` call re-reads before the end of the previous read: from the latest
/// line where nothing was open (an unanswered question, a reply still growing). A cursor
/// needing more is `cursor_invalid` — the caller reloads — rather than resumed with state lost.
pub const MAX_RESCAN_BYTES: u64 = 2 << 20;
/// A tool call's `summary` is at most this many characters.
pub const MAX_SUMMARY: usize = 120;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    User,
    Assistant,
    Command,
    System,
    Question,
    ToolCall,
}

/// Where an entry sits: the byte offset of the line that started it, and its index among
/// the entries that line produced. `sub == -1` is the point just before that line.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct Pos {
    pub off: u64,
    pub sub: i64,
}

/// One chat entry. Field names follow the web parser (`role`, `kind`, `text`, `ts`, `final`,
/// `queued`, `id`, `questions`).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Entry {
    pub cursor: String,
    pub role: &'static str,
    pub kind: Kind,
    /// RFC 3339, UTC.
    pub ts: Option<String>,
    pub text: String,
    #[serde(rename = "final", skip_serializing_if = "Option::is_none")]
    pub final_: Option<bool>,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub queued: bool,
    /// `command`: the command (`/model`, `!`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// `command`: its arguments, when it has any.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub args: Option<String>,
    /// `tool_call` / `question`: the tool-use id.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub questions: Option<Vec<Question>>,
    /// `question`: `pending` | `answered` | `declined`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<&'static str>,
    /// `question`: `{ answers }` / `{ declined: true }`, or JSON `null` while pending.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub answer: Option<Value>,

    #[serde(skip)]
    pub pos: Pos,
    /// Fingerprint of the entry's first line.
    #[serde(skip)]
    fp: u32,
    /// Offset just past the entry's first line.
    #[serde(skip)]
    line_end: u64,
    /// The latest clean line at or before the entry's first line, and Claude's prompt queue
    /// there: a parse may restart from it with that queue.
    #[serde(skip)]
    clean_at: u64,
    #[serde(skip)]
    clean_queue: Rc<QueueSnap>,
    /// Offset of the last line that changed the entry.
    #[serde(skip)]
    touched: u64,
}

impl Entry {
    fn new(role: &'static str, kind: Kind, text: String, ts: Option<String>) -> Self {
        Entry {
            cursor: String::new(),
            role,
            kind,
            ts,
            text,
            final_: None,
            queued: false,
            name: None,
            args: None,
            id: None,
            tool: None,
            summary: None,
            questions: None,
            status: None,
            answer: None,
            pos: Pos::default(),
            fp: 0,
            line_end: 0,
            clean_at: 0,
            clean_queue: Rc::default(),
            touched: 0,
        }
    }

    fn assistant(text: String, ts: Option<String>, final_: bool) -> Self {
        Entry {
            final_: Some(final_),
            ..Entry::new("assistant", Kind::Assistant, text, ts)
        }
    }

    fn tool_call(id: Option<String>, tool: String, summary: String, ts: Option<String>) -> Self {
        let text = if summary.is_empty() {
            tool.clone()
        } else {
            format!("{tool}: {summary}")
        };
        Entry {
            id,
            tool: Some(tool),
            summary: Some(summary),
            ..Entry::new("assistant", Kind::ToolCall, text, ts)
        }
    }

    fn user_side(c: Classified, ts: Option<String>) -> Self {
        Entry {
            name: c.name,
            args: c.args,
            ..Entry::new("user", c.kind, c.text, ts)
        }
    }

    /// This entry's own cursor (stable; carries no read hint).
    pub fn position_cursor(&self, fid: u32) -> Cursor {
        Cursor {
            pos: self.pos,
            fp: self.fp,
            fid,
            hint: None,
        }
    }
}

// ------------------------------------------------------------------ cursors

/// Claude's prompt queue at a point: each queued item's enqueue line and a hash of what it
/// queued (what a `remove` names it by), oldest first.
pub type QueueSnap = Vec<(u64, u32)>;

/// A read hint: what the call that minted the cursor had read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hint {
    /// Just past the last complete line read: everything before it has been observed.
    pub end: u64,
    /// A clean line at or before `end`: the next read starts here.
    pub rescan: u64,
    /// Fingerprint of the bytes just before `end`.
    pub end_fp: u32,
    /// Claude's prompt queue at `rescan`; `None` when it was too long to carry (the cursor
    /// then cannot resume: `--after` says `cursor_invalid` and the caller reloads).
    pub queue: Option<QueueSnap>,
    /// Minted by a `--limit`ed page: entries after the position were not delivered yet, so
    /// re-reading them is progress, not waste — the rescan bound then covers only the
    /// delivered part (from `rescan` to the position).
    pub paging: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cursor {
    pub pos: Pos,
    pub fp: u32,
    /// Which file: a hash of its device and inode (`0`: any — the empty cursor).
    pub fid: u32,
    pub hint: Option<Hint>,
}

const CURSOR_TAG: &str = "m2";
/// Queued prompts are not positions in the conversation; their cursors say so.
const QUEUED_TAG: &str = "q2";
/// The longest cursor printed (the contract's bound).
pub const MAX_CURSOR_LEN: usize = 512;

/// A transcript's identity: its device and inode, hashed. A transcript replaced by another
/// file (an atomic rewrite) has another.
pub fn file_id(meta: &std::fs::Metadata) -> u32 {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        fnv1a(format!("{}:{}", meta.dev(), meta.ino()).as_bytes()).max(1)
    }
    #[cfg(not(unix))]
    {
        let _ = meta;
        1
    }
}

impl Cursor {
    /// The cursor of an empty (or not yet written) transcript.
    pub fn empty() -> Self {
        Cursor {
            pos: Pos { off: 0, sub: -1 },
            fp: 0,
            fid: 0,
            hint: Some(Hint {
                end: 0,
                rescan: 0,
                end_fp: fnv1a(b""),
                queue: Some(Vec::new()),
                paging: false,
            }),
        }
    }

    pub fn is_empty_start(&self) -> bool {
        self.pos == (Pos { off: 0, sub: -1 }) && self.hint.as_ref().is_none_or(|h| h.end == 0)
    }

    pub fn encode(&self) -> String {
        let base = format!(
            "{CURSOR_TAG}.{:x}.{}.{:08x}.{:08x}",
            self.pos.off, self.pos.sub, self.fp, self.fid
        );
        let Some(h) = &self.hint else {
            return base;
        };
        let hinted = |queue: &str| {
            format!(
                "{base}.{:x}.{:x}.{:08x}.{queue}.{}",
                h.end,
                h.rescan,
                h.end_fp,
                if h.paging { "p" } else { "a" }
            )
        };
        let queue = match &h.queue {
            Some(q) => q
                .iter()
                .map(|(off, hash)| format!("{off:x}:{hash:08x}"))
                .collect::<Vec<_>>()
                .join(","),
            None => "!".into(),
        };
        let full = hinted(&queue);
        if full.len() <= MAX_CURSOR_LEN {
            full
        } else {
            hinted("!")
        }
    }

    /// `Err` with the reason when `s` is not a usable cursor.
    pub fn parse(s: &str) -> std::result::Result<Cursor, String> {
        let parts: Vec<&str> = s.trim().split('.').collect();
        if parts.first() == Some(&QUEUED_TAG) {
            return Err(
                "a queued prompt's cursor is not a position — page with the response's `cursor`"
                    .into(),
            );
        }
        let bad = || format!("not a fleet messages cursor: {s:?}");
        if parts.first() != Some(&CURSOR_TAG) || !(parts.len() == 5 || parts.len() == 10) {
            return Err(bad());
        }
        let hex = |p: &str| u64::from_str_radix(p, 16).map_err(|_| bad());
        let fp32 = |p: &str| u32::from_str_radix(p, 16).map_err(|_| bad());
        let pos = Pos {
            off: hex(parts[1])?,
            sub: parts[2].parse::<i64>().map_err(|_| bad())?,
        };
        if pos.sub < -1 {
            return Err(bad());
        }
        let fp = fp32(parts[3])?;
        let fid = fp32(parts[4])?;
        let hint = if parts.len() == 10 {
            let queue = match parts[8] {
                "!" => None,
                "" => Some(Vec::new()),
                q => Some(
                    q.split(',')
                        .map(|item| {
                            let (off, hash) = item.split_once(':').ok_or_else(bad)?;
                            Ok((hex(off)?, fp32(hash)?))
                        })
                        .collect::<std::result::Result<QueueSnap, String>>()?,
                ),
            };
            let h = Hint {
                end: hex(parts[5])?,
                rescan: hex(parts[6])?,
                end_fp: fp32(parts[7])?,
                queue,
                paging: match parts[9] {
                    "p" => true,
                    "a" => false,
                    _ => return Err(bad()),
                },
            };
            if h.rescan > h.end {
                return Err(bad());
            }
            Some(h)
        } else {
            None
        };
        Ok(Cursor { pos, fp, fid, hint })
    }
}

/// FNV-1a, 32 bits: a line's fingerprint.
pub fn fnv1a(bytes: &[u8]) -> u32 {
    let mut h: u32 = 0x811c_9dc5;
    for b in bytes {
        h ^= u32::from(*b);
        h = h.wrapping_mul(0x0100_0193);
    }
    h
}

// ------------------------------------------------------------------ text helpers

/// The `blockType` text parts of a message `content` (a string, or a list of blocks).
fn text_of(content: Option<&Value>, block_type: &str) -> String {
    match content {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Array(blocks)) => blocks
            .iter()
            .filter(|b| b.get("type").and_then(Value::as_str) == Some(block_type))
            .filter_map(|b| b.get("text").and_then(Value::as_str))
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    }
}

/// A transcript timestamp (ISO string, or epoch milliseconds) as RFC 3339 UTC.
fn rfc3339(v: Option<&Value>) -> Option<String> {
    let dt = match v? {
        Value::String(s) => chrono::DateTime::parse_from_rfc3339(s)
            .ok()?
            .with_timezone(&chrono::Utc),
        Value::Number(n) => chrono::DateTime::from_timestamp_millis(n.as_i64()?)?,
        _ => return None,
    };
    Some(dt.to_rfc3339_opts(chrono::SecondsFormat::Millis, true))
}

fn str_at<'a>(v: &'a Value, key: &str) -> Option<&'a str> {
    v.get(key).and_then(Value::as_str)
}

/// The inner text of the first `<name>…</name>`, trimmed.
fn tag(text: &str, name: &str) -> Option<String> {
    let open = format!("<{name}>");
    let close = format!("</{name}>");
    let start = text.find(&open)? + open.len();
    let len = text[start..].find(&close)?;
    Some(text[start..start + len].trim().to_string())
}

/// `text` without `<system-reminder>…</system-reminder>` blocks.
fn strip_reminders(text: &str) -> String {
    const OPEN: &str = "<system-reminder>";
    const CLOSE: &str = "</system-reminder>";
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(i) = rest.find(OPEN) {
        let Some(j) = rest[i..].find(CLOSE) else {
            break;
        };
        out.push_str(&rest[..i]);
        rest = &rest[i + j + CLOSE.len()..];
    }
    out.push_str(rest);
    out
}

/// What a user-side transcript text is, once classified.
#[derive(Debug, Clone, PartialEq)]
pub struct Classified {
    pub kind: Kind,
    pub text: String,
    pub name: Option<String>,
    pub args: Option<String>,
}

/// Classify one user-typed transcript entry (`classifyUserText`). `None`: not worth showing.
pub fn classify_user_text(raw: &str) -> Option<Classified> {
    let stripped = strip_reminders(raw);
    let text = stripped.trim();
    if text.is_empty() {
        return None;
    }
    const NOISE: [&str; 5] = [
        "<local-command-caveat>",
        "<local-command-stdout>",
        "<local-command-stderr>",
        "<bash-stdout>",
        "<bash-stderr>",
    ];
    if NOISE.iter().any(|n| text.starts_with(n)) {
        return None;
    }
    // Slash commands and skills: `<command-name>` / `<command-message>` / `<command-args>` in
    // either order.
    if text.starts_with("<command-name>") || text.starts_with("<command-message>") {
        let name = tag(text, "command-name").unwrap_or_else(|| {
            tag(text, "command-message")
                .filter(|m| !m.is_empty())
                .map(|m| format!("/{m}"))
                .unwrap_or_default()
        });
        let args = tag(text, "command-args").unwrap_or_default();
        let shown = if args.is_empty() {
            name.trim().to_string()
        } else {
            format!("{name} {args}").trim().to_string()
        };
        if shown.is_empty() {
            return None;
        }
        return Some(Classified {
            kind: Kind::Command,
            text: shown,
            name: Some(name),
            args: (!args.is_empty()).then_some(args),
        });
    }
    // `! cmd` typed into Claude Code (its output is dropped like local-command stdout).
    if text.starts_with("<bash-input>") {
        let cmd = tag(text, "bash-input").unwrap_or_default();
        if cmd.is_empty() {
            return None;
        }
        return Some(Classified {
            kind: Kind::Command,
            text: format!("! {cmd}"),
            name: Some("!".into()),
            args: Some(cmd),
        });
    }
    if text.starts_with("<task-notification>") {
        return Some(Classified {
            kind: Kind::System,
            text: tag(text, "summary").unwrap_or_else(|| "Background task finished".into()),
            name: None,
            args: None,
        });
    }
    Some(Classified {
        kind: Kind::User,
        text: text.to_string(),
        name: None,
        args: None,
    })
}

// ------------------------------------------------------------------ tool summaries

/// The first non-empty line of `s`, trimmed and capped at [`MAX_SUMMARY`] characters.
fn clip(s: &str) -> String {
    let line = s
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .unwrap_or("");
    if line.chars().count() <= MAX_SUMMARY {
        return line.to_string();
    }
    let mut out: String = line.chars().take(MAX_SUMMARY - 1).collect();
    out.push('…');
    out
}

/// A command given as a string or an argv list.
fn command_text(v: &Value) -> Option<String> {
    match v {
        Value::String(s) => Some(s.clone()),
        Value::Array(a) => {
            let parts: Vec<&str> = a.iter().filter_map(Value::as_str).collect();
            // `["bash", "-lc", "<script>"]`: the script is the command.
            if parts.len() == 3 && matches!(parts[1], "-lc" | "-c") {
                return Some(parts[2].to_string());
            }
            Some(parts.join(" "))
        }
        _ => None,
    }
    .filter(|s| !s.trim().is_empty())
}

/// Input keys a summary may come from when the tool is not one we know: one short argument,
/// never file contents.
const SUMMARY_KEYS: [&str; 15] = [
    "file_path",
    "notebook_path",
    "path",
    "command",
    "cmd",
    "pattern",
    "url",
    "query",
    "description",
    "skill",
    "task_name",
    "target",
    "name",
    "title",
    "subject",
];

/// One short argument of a tool call: a path, a command's first line, a pattern.
pub fn tool_summary(tool: &str, input: &Value) -> String {
    let s = |k: &str| {
        str_at(input, k)
            .filter(|v| !v.trim().is_empty())
            .map(str::to_string)
    };
    let count = |k: &str, unit: &str| {
        input
            .get(k)
            .and_then(Value::as_array)
            .map(|a| format!("{} {unit}", a.len()))
    };
    let picked = match tool {
        "Bash" | "exec_command" | "shell" | "shell_command" | "bash" | "local_shell" => input
            .get("command")
            .or_else(|| input.get("cmd"))
            .and_then(command_text),
        "Read" | "Write" | "Edit" | "MultiEdit" | "read" | "write" | "edit" => {
            s("file_path").or_else(|| s("path"))
        }
        "NotebookEdit" | "NotebookRead" => s("notebook_path"),
        "Grep" | "Glob" | "grep" | "glob" | "find" => s("pattern"),
        "WebFetch" => s("url"),
        "WebSearch" | "web_search" => s("query").or_else(|| {
            input
                .get("queries")
                .and_then(Value::as_array)
                .and_then(|q| q.first())
                .and_then(Value::as_str)
                .map(str::to_string)
        }),
        "Task" | "Agent" => s("description"),
        "Skill" => s("skill"),
        "TodoWrite" => count("todos", "todos"),
        "update_plan" => count("plan", "steps"),
        "spawn_agent" => s("task_name").or_else(|| s("message")),
        "send_message" | "followup_task" | "close_agent" => s("target"),
        "wait_agent" => input.get("targets").and_then(Value::as_array).map(|t| {
            t.iter()
                .filter_map(Value::as_str)
                .collect::<Vec<_>>()
                .join(", ")
        }),
        "write_stdin" => input.get("session_id").map(|v| match v {
            Value::String(s) => format!("session {s}"),
            other => format!("session {other}"),
        }),
        _ => None,
    };
    let picked = picked.or_else(|| {
        SUMMARY_KEYS
            .iter()
            .find_map(|k| input.get(*k).and_then(command_text))
    });
    picked.as_deref().map(clip).unwrap_or_default()
}

/// A Codex freeform tool call (`custom_tool_call`): `apply_patch` names its first file; any
/// other input shows its first line.
fn custom_summary(tool: &str, input: &str) -> String {
    if tool == "apply_patch" {
        for l in input.lines() {
            for p in ["*** Update File: ", "*** Add File: ", "*** Delete File: "] {
                if let Some(path) = l.strip_prefix(p) {
                    return clip(path);
                }
            }
        }
        return String::new();
    }
    match serde_json::from_str::<Value>(input) {
        Ok(v) if v.is_object() => tool_summary(tool, &v),
        _ => clip(input),
    }
}

// ------------------------------------------------------------------ parsers

/// What a parser needs to know about the line it is reading.
struct LineCtx {
    off: u64,
    fp: u32,
    line_end: u64,
}

trait LineParser {
    /// Read one parsed line; push new entries, update older ones (setting `touched`).
    fn line(&mut self, cx: &LineCtx, v: &Value, out: &mut Vec<Entry>);
    /// Nothing a later line could change is open: no question waiting for its result, no
    /// reply a later line could extend. (Claude's prompt queue is carried, not re-derived.)
    fn clean(&self) -> bool;
    /// Entries that belong at the end (Claude's still-queued prompts).
    fn finish(&self) -> Vec<Entry> {
        Vec::new()
    }
    /// Claude's prompt queue now.
    fn queue(&self) -> QueueSnap {
        Vec::new()
    }
    /// Bumped whenever the queue changes.
    fn queue_version(&self) -> u64 {
        0
    }
}

/// One item of Claude's prompt queue.
#[derive(Debug, Clone)]
struct QueuedPrompt {
    /// Hash of what it queued: a `remove` names the item by its content.
    hash: u32,
    /// What it shows as while still queued; `None` for what never shows (task
    /// notifications, a subagent's hand-back, an enqueue that records no content).
    shown: Option<String>,
    ts: Option<String>,
    off: u64,
    fp: u32,
    line_end: u64,
}

fn content_hash(content: Option<&Value>) -> u32 {
    match content {
        Some(v) => fnv1a(v.to_string().as_bytes()),
        None => fnv1a(b"\0none"),
    }
}

/// The text a queued prompt shows as, if it shows at all.
fn queued_text(content: Option<&Value>) -> Option<String> {
    let raw = text_of(content, "text");
    if raw.starts_with("<agent-message") {
        return None; // a subagent's hand-back, not something typed
    }
    classify_user_text(&raw)
        .filter(|c| c.kind == Kind::User && !c.text.trim().is_empty())
        .map(|c| c.text)
}

/// The queue item an `enqueue` line records.
fn queued_prompt(entry: &Value, cx: &LineCtx) -> QueuedPrompt {
    let content = entry.get("content");
    QueuedPrompt {
        hash: content_hash(content),
        shown: queued_text(content),
        ts: rfc3339(entry.get("timestamp")),
        off: cx.off,
        fp: cx.fp,
        line_end: cx.line_end,
    }
}

/// Claude Code: `~/.claude/projects/<cwd>/<session>.jsonl`.
#[derive(Default)]
struct Claude {
    /// The API message whose text the extendable assistant entry (`glue`) holds. Streamed
    /// messages arrive as one line per block — text, tool calls, more text — and the web
    /// parser glues a message's text blocks into one entry; so does this one, across the
    /// tool-call entries in between.
    last_assistant_id: Option<String>,
    glue: Option<usize>,
    /// tool_use id -> index of its question entry, until answered.
    asked: HashMap<String, usize>,
    /// Prompts typed while Claude works, oldest first — every enqueued item, shown or not,
    /// so a `dequeue` (FIFO) takes the right one.
    queue: VecDeque<QueuedPrompt>,
    queue_version: u64,
}

impl Claude {
    fn with_queue(queue: Vec<QueuedPrompt>) -> Self {
        Claude {
            queue: queue.into(),
            ..Claude::default()
        }
    }

    fn close_glue(&mut self) {
        self.glue = None;
        self.last_assistant_id = None;
    }
}

impl LineParser for Claude {
    fn line(&mut self, cx: &LineCtx, entry: &Value, out: &mut Vec<Entry>) {
        if !entry.is_object() || entry.get("isSidechain").is_some_and(truthy) {
            return;
        }
        let ts = rfc3339(entry.get("timestamp"));
        let ty = str_at(entry, "type").unwrap_or("");

        if ty == "queue-operation" {
            match str_at(entry, "operation") {
                Some("enqueue") => {
                    self.queue.push_back(queued_prompt(entry, cx));
                    self.queue_version += 1;
                }
                Some("dequeue") => {
                    if self.queue.pop_front().is_some() {
                        self.queue_version += 1;
                    }
                }
                Some("remove") => {
                    let hash = content_hash(entry.get("content"));
                    if let Some(i) = self.queue.iter().position(|q| q.hash == hash) {
                        self.queue.remove(i);
                        self.queue_version += 1;
                    }
                }
                _ => {}
            }
            return;
        }
        if ty == "attachment" {
            // A prompt typed while Claude worked and absorbed mid-turn.
            let Some(a) = entry.get("attachment") else {
                return;
            };
            let human = match a.get("origin") {
                Some(o) if truthy(o) => str_at(o, "kind") == Some("human"),
                _ => true,
            };
            if str_at(a, "type") != Some("queued_command")
                || str_at(a, "commandMode") != Some("prompt")
                || !human
            {
                return;
            }
            let Some(c) = classify_user_text(&text_of(a.get("prompt"), "text")) else {
                return;
            };
            if c.text.trim().is_empty() {
                return;
            }
            out.push(Entry::user_side(c, ts));
            self.close_glue();
            return;
        }

        let Some(msg) = entry.get("message").filter(|m| m.is_object()) else {
            return;
        };
        let content = msg.get("content");

        if ty == "user" {
            // A compaction writes its summary as a user entry, and a manual `/compact` also as
            // plain text just before it; the `<command-name>` entry after the summary is shown.
            if entry.get("isMeta").is_some_and(truthy)
                || entry.get("isCompactSummary").is_some_and(truthy)
                || content.and_then(Value::as_str) == Some("/compact")
            {
                return;
            }
            if !self.asked.is_empty()
                && let Some(blocks) = content.and_then(Value::as_array)
            {
                for b in blocks {
                    if str_at(b, "type") != Some("tool_result") {
                        continue;
                    }
                    let Some(idx) = str_at(b, "tool_use_id").and_then(|id| self.asked.remove(id))
                    else {
                        continue;
                    };
                    let q = &mut out[idx];
                    let outcome = questions::question_outcome(
                        q.questions.as_deref().unwrap_or(&[]),
                        entry,
                        b,
                    );
                    q.status = Some(outcome.status());
                    q.answer = Some(outcome.to_json());
                    q.touched = cx.off;
                }
            }
            let raw = text_of(content, "text");
            if raw.trim().is_empty() {
                // A pure tool_result: the message that called the tool has ended (its results
                // come after its last line), so nothing glues onto its text any more.
                self.close_glue();
                return;
            }
            let Some(c) = classify_user_text(&raw) else {
                return;
            };
            if matches!(c.kind, Kind::User | Kind::Command) {
                // A new prompt: a question still unanswered by now never gets a result — it
                // was abandoned (the session was interrupted or restarted on it).
                for (_, idx) in self.asked.drain() {
                    out[idx].status = Some("abandoned");
                    out[idx].touched = cx.off;
                }
            }
            out.push(Entry::user_side(c, ts));
            self.close_glue();
            return;
        }

        if ty == "assistant" {
            let msg_id = str_at(msg, "id").map(str::to_string);
            if msg_id.is_some() && msg_id != self.last_assistant_id {
                self.glue = None; // another API message: the previous one is complete
            }
            let body = text_of(content, "text");
            let body = body.trim();
            if !body.is_empty() {
                let final_ = str_at(msg, "stop_reason") != Some("tool_use");
                match self
                    .glue
                    .filter(|_| msg_id.is_some() && msg_id == self.last_assistant_id)
                {
                    Some(i) => {
                        let prev = &mut out[i];
                        prev.text = format!("{}\n\n{body}", prev.text);
                        prev.final_ = Some(prev.final_.unwrap_or(false) || final_);
                        if ts.is_some() {
                            prev.ts = ts.clone();
                        }
                        prev.touched = cx.off;
                    }
                    None => {
                        self.glue = msg_id.is_some().then_some(out.len());
                        out.push(Entry::assistant(body.to_string(), ts.clone(), final_));
                    }
                }
                self.last_assistant_id = msg_id;
            }
            for b in content.and_then(Value::as_array).into_iter().flatten() {
                if str_at(b, "type") != Some("tool_use") {
                    continue;
                }
                let name = str_at(b, "name").unwrap_or("");
                let id = str_at(b, "id").filter(|i| !i.is_empty());
                let input = b.get("input").unwrap_or(&Value::Null);
                if name == QUESTION_TOOL
                    && let Some(id) = id
                    && let Some(qs) = questions::normalize_questions(input)
                {
                    let mut e = Entry::new(
                        "assistant",
                        Kind::Question,
                        questions::questions_text(&qs),
                        ts.clone(),
                    );
                    e.id = Some(id.to_string());
                    e.questions = Some(qs);
                    e.status = Some("pending");
                    e.answer = Some(Value::Null);
                    self.asked.insert(id.to_string(), out.len());
                    out.push(e);
                    self.close_glue();
                } else if !name.is_empty() {
                    out.push(Entry::tool_call(
                        id.map(str::to_string),
                        name.to_string(),
                        tool_summary(name, input),
                        ts.clone(),
                    ));
                }
            }
        }
    }

    fn clean(&self) -> bool {
        self.asked.is_empty() && self.glue.is_none()
    }

    fn finish(&self) -> Vec<Entry> {
        let mut out = Vec::new();
        for q in &self.queue {
            let Some(text) = q.shown.clone() else {
                continue;
            };
            let mut e = Entry::new("user", Kind::User, text, q.ts.clone());
            e.queued = true;
            e.pos = Pos { off: q.off, sub: 0 };
            e.fp = q.fp;
            e.line_end = q.line_end;
            e.touched = q.off;
            out.push(e);
        }
        out
    }

    fn queue(&self) -> QueueSnap {
        self.queue.iter().map(|q| (q.off, q.hash)).collect()
    }

    fn queue_version(&self) -> u64 {
        self.queue_version
    }
}

/// Codex wraps its own context (AGENTS.md, environment, permissions) in user messages.
fn is_codex_context(text: &str) -> bool {
    text.starts_with('<') || text.starts_with("# AGENTS.md")
}

/// Codex rollout: `~/.codex/sessions/YYYY/MM/DD/rollout-*.jsonl`. Assistant text is `final`
/// unless a tool call follows it within the turn.
#[derive(Default)]
struct Codex {
    /// The turn's latest assistant message, until a tool call or a user turn.
    last_assistant: Option<usize>,
}

fn codex_tool_call(p: &Value, ts: Option<String>) -> Entry {
    let ptype = str_at(p, "type").unwrap_or("");
    let id = str_at(p, "call_id")
        .or_else(|| str_at(p, "id"))
        .map(str::to_string);
    let name = str_at(p, "name").map(str::to_string);
    let (tool, summary) = match ptype {
        "function_call" => {
            let tool = name.unwrap_or_else(|| "function".into());
            let args = match p.get("arguments") {
                Some(Value::String(s)) => serde_json::from_str(s).unwrap_or(Value::Null),
                Some(v) => v.clone(),
                None => Value::Null,
            };
            let summary = tool_summary(&tool, &args);
            (tool, summary)
        }
        "custom_tool_call" => {
            let tool = name.unwrap_or_else(|| "custom".into());
            let summary = custom_summary(&tool, str_at(p, "input").unwrap_or(""));
            (tool, summary)
        }
        _ => {
            // local_shell_call, web_search_call, …: the action says what it did.
            let tool = name.unwrap_or_else(|| ptype.trim_end_matches("_call").to_string());
            let action = p.get("action").unwrap_or(&Value::Null);
            (tool.clone(), tool_summary(&tool, action))
        }
    };
    Entry::tool_call(id, tool, summary, ts)
}

impl LineParser for Codex {
    fn line(&mut self, cx: &LineCtx, entry: &Value, out: &mut Vec<Entry>) {
        let Some(p) = entry.get("payload").filter(|p| p.is_object()) else {
            return;
        };
        if str_at(entry, "type") != Some("response_item") {
            return;
        }
        let ts = rfc3339(entry.get("timestamp"));
        let ptype = str_at(p, "type").unwrap_or("");
        if ptype == "message" {
            let role = str_at(p, "role").unwrap_or("");
            let block = if role == "user" {
                "input_text"
            } else {
                "output_text"
            };
            let body = text_of(p.get("content"), block);
            let body = body.trim();
            if body.is_empty() {
                return;
            }
            match role {
                "user" => {
                    if is_codex_context(body) {
                        return;
                    }
                    out.push(Entry::new("user", Kind::User, body.to_string(), ts));
                    self.last_assistant = None;
                }
                "assistant" => {
                    self.last_assistant = Some(out.len());
                    out.push(Entry::assistant(body.to_string(), ts, true));
                }
                _ => {}
            }
            return;
        }
        if ptype.ends_with("call") {
            if let Some(i) = self.last_assistant.take() {
                out[i].final_ = Some(false); // narration before a tool call
                out[i].touched = cx.off;
            }
            out.push(codex_tool_call(p, ts));
        }
    }

    fn clean(&self) -> bool {
        self.last_assistant.is_none()
    }
}

/// pi: `~/.pi/agent/sessions/<cwd>/*.jsonl`.
#[derive(Default)]
struct Pi;

impl LineParser for Pi {
    fn line(&mut self, _cx: &LineCtx, entry: &Value, out: &mut Vec<Entry>) {
        if str_at(entry, "type") != Some("message") {
            return;
        }
        let Some(msg) = entry.get("message").filter(|m| m.is_object()) else {
            return;
        };
        let ts = rfc3339(entry.get("timestamp")).or_else(|| rfc3339(msg.get("timestamp")));
        let body = text_of(msg.get("content"), "text");
        let body = body.trim();
        match str_at(msg, "role") {
            Some("user") if !body.is_empty() => {
                out.push(Entry::new("user", Kind::User, body.to_string(), ts));
            }
            Some("assistant") => {
                if !body.is_empty() {
                    let final_ = str_at(msg, "stopReason") != Some("toolUse");
                    out.push(Entry::assistant(body.to_string(), ts.clone(), final_));
                }
                for b in msg
                    .get("content")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                {
                    if str_at(b, "type") != Some("toolCall") {
                        continue;
                    }
                    let Some(name) = str_at(b, "name").filter(|n| !n.is_empty()) else {
                        continue;
                    };
                    let args = b.get("arguments").unwrap_or(&Value::Null);
                    out.push(Entry::tool_call(
                        str_at(b, "id").map(str::to_string),
                        name.to_string(),
                        tool_summary(name, args),
                        ts.clone(),
                    ));
                }
            }
            _ => {}
        }
    }

    fn clean(&self) -> bool {
        true
    }
}

/// Just enough of a line to tell whether its parser reads it. The lines none of them read —
/// Claude's API request logs and hook output, Codex's tool output and reasoning — are often
/// most of a transcript's bytes; they are skimmed without building a `Value`.
#[derive(Deserialize)]
struct Head<'a> {
    #[serde(rename = "type", borrow, default)]
    ty: Option<Cow<'a, str>>,
    #[serde(borrow, default)]
    attachment: Option<Typed<'a>>,
    #[serde(borrow, default)]
    payload: Option<Typed<'a>>,
}

#[derive(Deserialize)]
struct Typed<'a> {
    #[serde(rename = "type", borrow, default)]
    ty: Option<Cow<'a, str>>,
}

fn wanted(h: Harness, line: &[u8]) -> bool {
    let Ok(head) = serde_json::from_slice::<Head>(line) else {
        return true; // an unexpected shape: let the parser decide
    };
    let ty = head.ty.as_deref().unwrap_or("");
    let sub = |t: &Option<Typed>| {
        t.as_ref()
            .and_then(|t| t.ty.as_deref())
            .unwrap_or("")
            .to_string()
    };
    match h {
        Harness::Claude => {
            matches!(ty, "user" | "assistant" | "queue-operation")
                || (ty == "attachment" && sub(&head.attachment) == "queued_command")
        }
        Harness::Codex => {
            let p = sub(&head.payload);
            ty == "response_item" && (p == "message" || p.ends_with("call"))
        }
        Harness::Pi => ty == "message",
    }
}

fn parser_for(h: Harness, queue: Vec<QueuedPrompt>) -> Box<dyn LineParser> {
    match h {
        Harness::Claude => Box::new(Claude::with_queue(queue)),
        Harness::Codex => Box::new(Codex::default()),
        Harness::Pi => Box::new(Pi),
    }
}

/// The entries of a run of transcript lines.
#[derive(Debug, Default)]
pub struct Parsed {
    /// In transcript order.
    pub entries: Vec<Entry>,
    /// Prompts still queued at the end (Claude), oldest first.
    pub queued: Vec<Entry>,
    /// Just past the last complete line.
    pub end: u64,
    /// The latest clean line start at or before `end` (`end` itself when nothing is open),
    /// and Claude's prompt queue there.
    pub clean_at_end: u64,
    pub clean_queue_end: Rc<QueueSnap>,
}

/// A parse in progress: fed runs of lines, it can go on past where its caller's interest
/// ends (to settle what is still open there).
struct Engine {
    h: Harness,
    p: Box<dyn LineParser>,
    out: Vec<Entry>,
    last_clean: u64,
    clean_queue: Rc<QueueSnap>,
    snapped: u64,
}

impl Engine {
    fn new(h: Harness, base: u64, queue: Vec<QueuedPrompt>) -> Self {
        let p = parser_for(h, queue);
        Engine {
            h,
            clean_queue: Rc::new(p.queue()),
            snapped: p.queue_version(),
            p,
            out: Vec::new(),
            last_clean: base,
        }
    }

    /// Read the complete lines of `buf` (starting at byte `base`, a line start). Returns the
    /// offset just past the last complete line.
    fn feed(&mut self, buf: &[u8], base: u64) -> u64 {
        let mut at = 0usize;
        while let Some(nl) = buf[at..].iter().position(|&b| b == b'\n') {
            let start = at;
            at = start + nl + 1;
            let off = base + start as u64;
            if self.p.clean() {
                self.last_clean = off;
                if self.p.queue_version() != self.snapped {
                    self.clean_queue = Rc::new(self.p.queue());
                    self.snapped = self.p.queue_version();
                }
            }
            let raw = &buf[start..start + nl];
            let line = raw.strip_suffix(b"\r").unwrap_or(raw);
            if line.iter().all(u8::is_ascii_whitespace) || !wanted(self.h, line) {
                continue;
            }
            let Ok(v) = serde_json::from_slice::<Value>(line) else {
                continue;
            };
            let cx = LineCtx {
                off,
                fp: fnv1a(raw),
                line_end: base + at as u64,
            };
            let before = self.out.len();
            self.p.line(&cx, &v, &mut self.out);
            for (i, e) in self.out[before..].iter_mut().enumerate() {
                e.pos = Pos { off, sub: i as i64 };
                e.fp = cx.fp;
                e.line_end = cx.line_end;
                e.clean_at = self.last_clean;
                e.clean_queue = self.clean_queue.clone();
                e.touched = off;
            }
        }
        base + at as u64
    }

    fn finish(self, end: u64) -> Parsed {
        let (clean_at_end, clean_queue_end) = if self.p.clean() {
            (end, Rc::new(self.p.queue()))
        } else {
            (self.last_clean, self.clean_queue)
        };
        Parsed {
            clean_at_end,
            clean_queue_end,
            queued: self.p.finish(),
            entries: self.out,
            end,
        }
    }
}

/// Parse the complete lines of `buf`, which starts at byte `base` of the transcript and at a
/// line start. A trailing line without its newline is left for the next read.
pub fn parse(h: Harness, buf: &[u8], base: u64) -> Parsed {
    parse_from(h, buf, base, Vec::new())
}

/// [`parse`], starting with Claude's prompt queue as it was at `base`.
fn parse_from(h: Harness, buf: &[u8], base: u64, queue: Vec<QueuedPrompt>) -> Parsed {
    let mut e = Engine::new(h, base, queue);
    let end = e.feed(buf, base);
    e.finish(end)
}

// ------------------------------------------------------------------ reading

/// One page of entries, as `fleet messages` prints it.
#[derive(Debug, Default)]
pub struct Page {
    pub entries: Vec<Entry>,
    /// `--after`: older entries that changed since the cursor's read (a question answered,
    /// streamed text glued on, narration a tool call turned non-final).
    pub updated: Vec<Entry>,
    pub has_more_before: bool,
    pub has_more_after: bool,
    /// Where to page or poll from: the newest entry's position (plus a read hint), the
    /// `--after` cursor when nothing is newer, `None` when there is nothing to point at.
    pub cursor: Option<String>,
    /// How many transcript bytes this call read.
    pub bytes_read: u64,
}

#[derive(Debug)]
pub enum ReadError {
    /// The cursor does not fit this transcript (not found, the transcript was rewritten or
    /// replaced, or it cannot be resumed exactly).
    CursorInvalid(String),
    Io(io::Error),
}

impl From<io::Error> for ReadError {
    fn from(e: io::Error) -> Self {
        ReadError::Io(e)
    }
}

impl std::fmt::Display for ReadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ReadError::CursorInvalid(m) => f.write_str(m),
            ReadError::Io(e) => write!(f, "{e}"),
        }
    }
}

fn invalid<T>(msg: &str) -> Result<T, ReadError> {
    Err(ReadError::CursorInvalid(msg.to_string()))
}

fn read_range(f: &mut File, start: u64, end: u64) -> io::Result<Vec<u8>> {
    let mut buf = Vec::with_capacity(end.saturating_sub(start) as usize);
    f.seek(SeekFrom::Start(start))?;
    f.take(end.saturating_sub(start)).read_to_end(&mut buf)?;
    Ok(buf)
}

/// `[start, end)` starting at a line boundary: a partial first line is dropped. Returns where
/// the kept bytes start.
fn read_from_line(f: &mut File, start: u64, end: u64) -> io::Result<(u64, Vec<u8>)> {
    if start == 0 {
        return Ok((0, read_range(f, 0, end)?));
    }
    // One byte earlier: a newline there means `start` is already a line start.
    let mut buf = read_range(f, start - 1, end)?;
    match buf.iter().position(|&b| b == b'\n') {
        Some(i) => {
            buf.drain(..=i);
            Ok((start + i as u64, buf))
        }
        None => Ok((end, Vec::new())),
    }
}

/// The line starting at `off`, with its newline (none when it is incomplete).
fn read_line_at(f: &mut File, off: u64) -> io::Result<Vec<u8>> {
    f.seek(SeekFrom::Start(off))?;
    let mut line = Vec::new();
    BufReader::new(&mut *f).read_until(b'\n', &mut line)?;
    Ok(line)
}

fn end_fp(f: &mut File, end: u64) -> io::Result<u32> {
    Ok(fnv1a(&read_range(f, end.saturating_sub(256), end)?))
}

/// The line starting at `off` must be the cursor's: a line start, complete, same fingerprint.
/// Returns the offset just past it.
fn check_line(f: &mut File, off: u64, fp: u32, len: u64) -> Result<u64, ReadError> {
    check_line_start(f, off, len)?;
    if off >= len {
        return invalid("cursor is past the end of the transcript");
    }
    let line = read_line_at(f, off)?;
    if line.last() != Some(&b'\n') {
        return invalid("cursor does not point at a complete line");
    }
    if fnv1a(&line[..line.len() - 1]) != fp {
        return invalid("the transcript changed under the cursor (rewritten)");
    }
    Ok(off + line.len() as u64)
}

fn check_line_start(f: &mut File, off: u64, len: u64) -> Result<(), ReadError> {
    if off > len {
        return invalid("cursor is past the end of the transcript");
    }
    if off > 0 && read_range(f, off - 1, off)?.first() != Some(&b'\n') {
        return invalid("cursor does not point at a line start");
    }
    Ok(())
}

/// Claude's prompt queue from a hint, re-read from its enqueue lines (each must still be the
/// enqueue of the same content).
fn load_queue(f: &mut File, snap: &QueueSnap, len: u64) -> Result<Vec<QueuedPrompt>, ReadError> {
    let mut out = Vec::with_capacity(snap.len());
    for &(off, hash) in snap {
        check_line_start(f, off, len)?;
        let raw = read_line_at(f, off)?;
        let Some(line) = raw.strip_suffix(b"\n") else {
            return invalid("a queued prompt's line is gone (rewritten)");
        };
        let v: Value = serde_json::from_slice(line.strip_suffix(b"\r").unwrap_or(line))
            .map_err(|_| ReadError::CursorInvalid("a queued prompt's line changed".into()))?;
        if str_at(&v, "operation") != Some("enqueue") || content_hash(v.get("content")) != hash {
            return invalid("a queued prompt's line changed (rewritten)");
        }
        let cx = LineCtx {
            off,
            fp: fnv1a(line),
            line_end: off + raw.len() as u64,
        };
        out.push(queued_prompt(&v, &cx));
    }
    Ok(out)
}

fn hint(f: &mut File, end: u64, rescan: u64, queue: &QueueSnap, paging: bool) -> io::Result<Hint> {
    Ok(Hint {
        end,
        rescan,
        end_fp: end_fp(f, end)?,
        queue: Some(queue.clone()),
        paging,
    })
}

fn stamp(entries: &mut [Entry], fid: u32) {
    for e in entries {
        e.cursor = if e.queued {
            format!("{QUEUED_TAG}.{:x}.{}.{:08x}", e.pos.off, e.pos.sub, e.fp)
        } else {
            e.position_cursor(fid).encode()
        };
    }
}

/// The newest `limit` entries (no cursor), growing the tail until it holds them.
pub fn latest(path: &Path, h: Harness, limit: usize) -> io::Result<Page> {
    let mut f = File::open(path)?;
    let meta = f.metadata()?;
    let (len, fid) = (meta.len(), file_id(&meta));
    let mut bytes = TAIL_BYTES;
    loop {
        let start = len.saturating_sub(bytes);
        let (base, buf) = read_from_line(&mut f, start, len)?;
        let parsed = parse(h, &buf, base);
        let n = parsed.entries.len();
        if start == 0 || n > limit || bytes >= MAX_GROWN_TAIL_BYTES {
            let mut entries = parsed.entries;
            entries.drain(..n.saturating_sub(limit));
            let (pos, fp) = entries.last().map_or(
                (
                    Pos {
                        off: parsed.end,
                        sub: -1,
                    },
                    0,
                ),
                |e| (e.pos, e.fp),
            );
            let cursor = Cursor {
                pos,
                fp,
                fid,
                hint: Some(hint(
                    &mut f,
                    parsed.end,
                    parsed.clean_at_end,
                    &parsed.clean_queue_end,
                    false,
                )?),
            };
            entries.extend(parsed.queued);
            stamp(&mut entries, fid);
            // Nothing to point at: the caller loads without a cursor again (cheap — the
            // transcript holds no entries).
            let cursor = (!entries.is_empty()).then(|| cursor.encode());
            return Ok(Page {
                entries,
                updated: Vec::new(),
                has_more_before: n > limit || base > 0,
                has_more_after: false,
                cursor,
                bytes_read: len - start,
            });
        }
        bytes = (bytes * 2).min(MAX_GROWN_TAIL_BYTES);
    }
}

/// Entries strictly newer than `c`, oldest first; `limit` caps them.
///
/// With the page cursor's read hint the parse restarts exactly (Claude's prompt queue comes
/// with it) from its clean line — refused as `cursor_invalid` when that line lies more than
/// [`MAX_RESCAN_BYTES`] before the end of the last read. An entry's own cursor (no hint)
/// restarts at its line, without the queue state before it.
pub fn after(path: &Path, h: Harness, c: &Cursor, limit: Option<usize>) -> Result<Page, ReadError> {
    let mut f = File::open(path)?;
    let meta = f.metadata()?;
    let (len, fid) = (meta.len(), file_id(&meta));
    if c.fid != 0 && c.fid != fid {
        return invalid("the transcript was replaced by another file");
    }
    // The anchor line, always: an edit inside it is a rewrite even when its length held.
    let anchor_end = if c.pos.sub >= 0 {
        check_line(&mut f, c.pos.off, c.fp, len)?
    } else {
        check_line_start(&mut f, c.pos.off, len)?;
        c.pos.off
    };
    let (from, seen_until, queue) = match &c.hint {
        Some(hint) => {
            if hint.end > len {
                return invalid("the transcript is shorter than the cursor (rewritten)");
            }
            if end_fp(&mut f, hint.end)? != hint.end_fp {
                return invalid("the transcript changed under the cursor (rewritten)");
            }
            let Some(q) = &hint.queue else {
                return invalid("too many queued prompts to resume exactly — reload");
            };
            // Re-reading what was delivered (to resume what is open there) is bounded; reading
            // entries a limited page has not delivered yet is not.
            let replay = if hint.paging {
                c.pos.off.saturating_sub(hint.rescan)
            } else {
                hint.end - hint.rescan
            };
            if replay > MAX_RESCAN_BYTES {
                return invalid(
                    "the open state (an unanswered question, a reply in progress) is too far back to resume exactly — reload",
                );
            }
            (hint.rescan, hint.end, load_queue(&mut f, q, len)?)
        }
        None => (c.pos.off, anchor_end, Vec::new()),
    };
    let buf = read_range(&mut f, from, len)?;
    let parsed = parse_from(h, &buf, from, queue);
    if c.pos.sub >= 0
        && from <= c.pos.off
        && !parsed
            .entries
            .iter()
            .any(|e| e.pos == c.pos && e.fp == c.fp)
    {
        return invalid("no entry at the cursor (rewritten transcript, or not this session's)");
    }
    let mut entries = Vec::new();
    let mut updated = Vec::new();
    for e in parsed.entries {
        if e.pos > c.pos {
            entries.push(e);
        } else if e.touched >= seen_until {
            updated.push(e);
        }
    }
    let has_more_after = limit.is_some_and(|l| entries.len() > l);
    // The read hint keeps two things apart: where the next page starts (the position, and a
    // clean line before it) and how far this call observed (`end`), so an update reported now
    // is not reported again.
    let cursor = if has_more_after {
        entries.truncate(limit.unwrap_or_default());
        let last = entries.last().expect("a limit of at least 1");
        Cursor {
            pos: last.pos,
            fp: last.fp,
            fid,
            hint: Some(hint(
                &mut f,
                parsed.end,
                last.clean_at,
                &last.clean_queue,
                true,
            )?),
        }
    } else {
        let (pos, fp) = entries.last().map_or((c.pos, c.fp), |e| (e.pos, e.fp));
        Cursor {
            pos,
            fp,
            fid,
            hint: Some(hint(
                &mut f,
                parsed.end,
                parsed.clean_at_end,
                &parsed.clean_queue_end,
                false,
            )?),
        }
    };
    // The whole current queue, every time (the caller replaces what it showed).
    entries.extend(parsed.queued);
    stamp(&mut entries, fid);
    stamp(&mut updated, fid);
    Ok(Page {
        entries,
        updated,
        has_more_before: c.pos.sub >= 0 || c.pos.off > 0,
        has_more_after,
        cursor: Some(cursor.encode()),
        bytes_read: len - from,
    })
}

/// Up to `limit` entries strictly older than `c`, oldest first.
pub fn before(path: &Path, h: Harness, c: &Cursor, limit: usize) -> Result<Page, ReadError> {
    let mut f = File::open(path)?;
    let meta = f.metadata()?;
    let (len, fid) = (meta.len(), file_id(&meta));
    if c.fid != 0 && c.fid != fid {
        return invalid("the transcript was replaced by another file");
    }
    let line_end = if c.pos.sub >= 0 {
        check_line(&mut f, c.pos.off, c.fp, len)?
    } else {
        check_line_start(&mut f, c.pos.off, len)?;
        c.pos.off
    };
    let mut bytes = TAIL_BYTES;
    loop {
        let start = line_end.saturating_sub(bytes);
        let (base, buf) = read_from_line(&mut f, start, line_end)?;
        let mut engine = Engine::new(h, base, Vec::new());
        engine.feed(&buf, base);
        let n = engine.out.iter().filter(|e| e.pos < c.pos).count();
        if start == 0 || n > limit || bytes >= MAX_BEFORE_BYTES {
            let mut read = line_end - start;
            read += settle(&mut f, &mut engine, line_end, len)?;
            let mut older: Vec<Entry> = engine.out.into_iter().filter(|e| e.pos < c.pos).collect();
            older.drain(..n.saturating_sub(limit));
            stamp(&mut older, fid);
            let cursor = older.last().map(|e| e.position_cursor(fid).encode());
            return Ok(Page {
                entries: older,
                updated: Vec::new(),
                has_more_before: n > limit || base > 0,
                has_more_after: true,
                cursor,
                bytes_read: read,
            });
        }
        bytes = (bytes * 2).min(MAX_BEFORE_BYTES);
    }
}

/// What is still open where a `--before` window ends settles after it: the later blocks of a
/// reply (glued into its entry), a question's result (or the prompt that abandoned it), the
/// tool call that turns Codex narration non-final. Read on from `from` until nothing is open
/// (at most [`MAX_BEFORE_BYTES`]). Returns the bytes read.
fn settle(f: &mut File, engine: &mut Engine, from: u64, len: u64) -> io::Result<u64> {
    let mut at = from;
    let mut chunk = 1u64 << 20;
    while !engine.p.clean() && at < len && at - from < MAX_BEFORE_BYTES {
        let end = (at + chunk).min(len);
        let buf = read_range(f, at, end)?;
        let next = engine.feed(&buf, at);
        if next == at {
            if end == len {
                break; // a last line still being written
            }
            chunk *= 2; // one line longer than the chunk
            continue;
        }
        at = next;
    }
    Ok(at - from)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn jl(v: &[Value]) -> String {
        v.iter().map(|x| format!("{x}\n")).collect()
    }
    fn user(content: Value) -> Value {
        json!({ "type": "user", "timestamp": "2026-09-18T18:01:52.017Z", "message": { "role": "user", "content": content } })
    }
    fn user_x(content: Value, extra: Value) -> Value {
        let mut u = user(content);
        for (k, v) in extra.as_object().unwrap() {
            u[k] = v.clone();
        }
        u
    }
    fn asst(content: Value, id: &str, stop: &str) -> Value {
        json!({ "type": "assistant", "timestamp": "2026-09-18T18:01:57.000Z",
                "message": { "id": id, "role": "assistant", "content": content, "stop_reason": stop } })
    }
    fn claude(text: &str) -> Vec<Entry> {
        let p = parse(Harness::Claude, text.as_bytes(), 0);
        p.entries.into_iter().chain(p.queued).collect()
    }
    fn shape(es: &[Entry]) -> Vec<(&'static str, Kind, String, Option<bool>)> {
        es.iter()
            .map(|e| (e.role, e.kind, e.text.clone(), e.final_))
            .collect()
    }

    #[test]
    fn claude_keeps_prompts_text_and_tool_calls_drops_thinking_results_meta_hooks() {
        let text = jl(&[
            user(json!("Hello there")),
            asst(
                json!([{ "type": "thinking", "thinking": "hmm" }]),
                "m1",
                "tool_use",
            ),
            asst(
                json!([{ "type": "text", "text": "Working on it." }]),
                "m1",
                "tool_use",
            ),
            asst(
                json!([{ "type": "tool_use", "id": "t1", "name": "Bash", "input": { "command": "git status\ngit log", "description": "x" } }]),
                "m1",
                "tool_use",
            ),
            user(
                json!([{ "type": "tool_result", "tool_use_id": "t1", "content": "SECRET OUTPUT" }]),
            ),
            asst(
                json!([{ "type": "text", "text": "Done." }]),
                "m2",
                "end_turn",
            ),
            user_x(json!("meta stuff"), json!({ "isMeta": true })),
            json!({ "type": "attachment", "attachment": { "type": "hook_success", "content": "hook said" } }),
            json!({ "type": "system", "subtype": "stop_hook_summary" }),
        ]) + "not json\n";
        let es = claude(&text);
        assert_eq!(
            shape(&es),
            vec![
                ("user", Kind::User, "Hello there".into(), None),
                (
                    "assistant",
                    Kind::Assistant,
                    "Working on it.".into(),
                    Some(false)
                ),
                ("assistant", Kind::ToolCall, "Bash: git status".into(), None),
                ("assistant", Kind::Assistant, "Done.".into(), Some(true)),
            ]
        );
        assert_eq!(es[2].id.as_deref(), Some("t1"));
        assert_eq!(es[2].tool.as_deref(), Some("Bash"));
        assert_eq!(es[2].summary.as_deref(), Some("git status"));
        assert_eq!(es[0].ts.as_deref(), Some("2026-09-18T18:01:52.017Z"));
        let all = serde_json::to_string(&es).unwrap();
        assert!(!all.contains("SECRET") && !all.contains("hmm") && !all.contains("hook said"));
    }

    #[test]
    fn claude_glues_streamed_blocks_and_skips_sidechains() {
        let text = jl(&[
            user(json!("q")),
            asst(
                json!([{ "type": "text", "text": "part one" }]),
                "same",
                "tool_use",
            ),
            asst(
                json!([{ "type": "text", "text": "part two" }]),
                "same",
                "end_turn",
            ),
            json!({ "type": "assistant", "isSidechain": true, "message": { "id": "side", "role": "assistant", "content": [{ "type": "text", "text": "subagent" }] } }),
            json!({ "type": "user", "isSidechain": true, "message": { "role": "user", "content": "subagent prompt" } }),
        ]);
        let es = claude(&text);
        assert_eq!(es.len(), 2);
        assert_eq!(es[1].text, "part one\n\npart two");
        assert_eq!(es[1].final_, Some(true));
    }

    #[test]
    fn claude_shows_a_compaction_once() {
        let text = jl(&[
            user(json!("Hello there")),
            asst(
                json!([{ "type": "text", "text": "Done." }]),
                "m1",
                "end_turn",
            ),
            user(json!("/compact")),
            json!({ "type": "system", "subtype": "compact_boundary", "content": "Conversation compacted" }),
            user_x(
                json!("This session is being continued."),
                json!({ "isCompactSummary": true }),
            ),
            user(json!(
                "<command-name>/compact</command-name>\n<command-message>compact</command-message>\n<command-args></command-args>"
            )),
            user(json!("Next")),
        ]);
        let es = claude(&text);
        assert_eq!(
            es.iter()
                .map(|e| (e.role, e.kind, e.text.as_str()))
                .collect::<Vec<_>>(),
            vec![
                ("user", Kind::User, "Hello there"),
                ("assistant", Kind::Assistant, "Done."),
                ("user", Kind::Command, "/compact"),
                ("user", Kind::User, "Next"),
            ]
        );
    }

    #[test]
    fn classify_handles_commands_notifications_noise_and_reminders() {
        let c = classify_user_text("<command-name>/model</command-name>\n<command-message>model</command-message>\n<command-args>opus</command-args>").unwrap();
        assert_eq!(
            (
                c.kind,
                c.text.as_str(),
                c.name.as_deref(),
                c.args.as_deref()
            ),
            (Kind::Command, "/model opus", Some("/model"), Some("opus"))
        );
        let c = classify_user_text("<command-message>p-dev:overview</command-message>\n<command-name>/p-dev:overview</command-name>\n<command-args>onboard me https://example.com/pull/1</command-args>").unwrap();
        assert_eq!(
            c.text,
            "/p-dev:overview onboard me https://example.com/pull/1"
        );
        let c = classify_user_text("<command-name>/clear</command-name>\n            <command-message>clear</command-message>\n            <command-args></command-args>").unwrap();
        assert_eq!((c.text.as_str(), c.args), ("/clear", None));
        let c = classify_user_text("<command-message>review</command-message>").unwrap();
        assert_eq!(c.text, "/review");
        let c = classify_user_text("<bash-input> git status</bash-input>").unwrap();
        assert_eq!(
            (
                c.kind,
                c.text.as_str(),
                c.name.as_deref(),
                c.args.as_deref()
            ),
            (Kind::Command, "! git status", Some("!"), Some("git status"))
        );
        assert_eq!(
            classify_user_text("<bash-stdout>clean</bash-stdout><bash-stderr></bash-stderr>"),
            None
        );
        let c = classify_user_text("<task-notification>\n<task-id>x</task-id>\n<summary>Agent \"Build\" finished</summary>\n</task-notification>").unwrap();
        assert_eq!(
            (c.kind, c.text.as_str()),
            (Kind::System, "Agent \"Build\" finished")
        );
        assert_eq!(
            classify_user_text("<local-command-caveat>Caveat</local-command-caveat>"),
            None
        );
        assert_eq!(
            classify_user_text("<local-command-stdout>ok</local-command-stdout>"),
            None
        );
        assert_eq!(
            classify_user_text("<system-reminder>ignore me</system-reminder>"),
            None
        );
        let c =
            classify_user_text("<system-reminder>ctx</system-reminder>\nReal question?").unwrap();
        assert_eq!((c.kind, c.text.as_str()), (Kind::User, "Real question?"));
    }

    fn question_input() -> Value {
        json!({ "questions": [{ "question": "Merge it?", "header": "Merge", "multiSelect": false,
                "options": [{ "label": "Yes", "description": "squash" }, { "label": "No" }] }] })
    }

    #[test]
    fn claude_questions_carry_their_outcome() {
        let input = question_input();
        let text = jl(&[
            asst(
                json!([{ "type": "text", "text": "Before I go on:" }]),
                "mq",
                "tool_use",
            ),
            asst(
                json!([{ "type": "tool_use", "id": "tq1", "name": "AskUserQuestion", "input": input }]),
                "mq",
                "tool_use",
            ),
            json!({ "type": "user", "message": { "role": "user", "content": [{ "type": "tool_result", "tool_use_id": "tq1", "content": "answered" }] },
                    "toolUseResult": { "questions": input["questions"], "answers": { "Merge it?": "Yes" } } }),
            asst(
                json!([{ "type": "tool_use", "id": "tq2", "name": "AskUserQuestion", "input": input }]),
                "mr",
                "tool_use",
            ),
            user(
                json!([{ "type": "tool_result", "tool_use_id": "tq2", "content": "rejected", "is_error": true }]),
            ),
            asst(
                json!([{ "type": "tool_use", "id": "tq3", "name": "AskUserQuestion", "input": input }]),
                "ms",
                "tool_use",
            ),
        ]);
        let es = claude(&text);
        assert_eq!(
            es.iter()
                .map(|e| (e.kind, e.id.clone()))
                .collect::<Vec<_>>(),
            vec![
                (Kind::Assistant, None),
                (Kind::Question, Some("tq1".into())),
                (Kind::Question, Some("tq2".into())),
                (Kind::Question, Some("tq3".into())),
            ]
        );
        assert_eq!(es[1].status, Some("answered"));
        assert_eq!(
            es[1].answer,
            Some(json!({ "answers": ["Yes"], "picked": [[0]] }))
        );
        assert_eq!(
            es[1].questions.as_ref().unwrap()[0].options[0].description,
            "squash"
        );
        assert!(es[1].text.contains("**Merge: Merge it?**\n- Yes\n- No"));
        assert_eq!(es[2].status, Some("declined"));
        assert_eq!(es[2].answer, Some(json!({ "declined": true })));
        assert_eq!(es[3].status, Some("pending"));
        let v = serde_json::to_value(&es[3]).unwrap();
        assert_eq!(v["answer"], Value::Null);
        assert!(v.as_object().unwrap().contains_key("answer"));
    }

    #[test]
    fn claude_queued_prompts_absorbed_in_place_still_waiting_last() {
        let q = |op: &str, content: Option<&str>| {
            let mut v = json!({ "type": "queue-operation", "operation": op, "timestamp": "2026-01-01T00:00:00Z" });
            if let Some(c) = content {
                v["content"] = json!(c);
            }
            v
        };
        let absorbed = |prompt: &str, origin: Value| {
            json!({ "type": "attachment", "timestamp": "2026-01-01T00:00:01Z",
            "attachment": { "type": "queued_command", "prompt": prompt, "commandMode": "prompt", "origin": origin } })
        };
        let text = jl(&[
            user(json!("Start")),
            q("enqueue", Some("first")),
            q(
                "enqueue",
                Some("<task-notification><summary>done</summary></task-notification>"),
            ),
            q("remove", Some("first")),
            absorbed("first", json!({ "kind": "human" })),
            absorbed("from a peer", json!({ "kind": "peer" })),
            json!({ "type": "attachment", "attachment": { "type": "queued_command", "prompt": "note", "commandMode": "task-notification" } }),
            q("dequeue", None),
            q("enqueue", Some("second")),
            q(
                "enqueue",
                Some("<agent-message from=\"x\">hand-back</agent-message>"),
            ),
            q("enqueue", Some("third")),
            q("dequeue", None),
            user(json!("second")),
            asst(
                json!([{ "type": "text", "text": "Working." }]),
                "m1",
                "tool_use",
            ),
        ]);
        let es = claude(&text);
        assert_eq!(
            es.iter()
                .map(|e| (e.text.as_str(), e.queued))
                .collect::<Vec<_>>(),
            vec![
                ("Start", false),
                ("first", false),
                ("second", false),
                ("Working.", false),
                ("third", true),
            ]
        );
        assert_eq!(
            es.last().unwrap().ts.as_deref(),
            Some("2026-01-01T00:00:00.000Z")
        );
    }

    #[test]
    fn codex_prompts_replies_tool_calls_context_dropped() {
        let msg = |role: &str, ty: &str, text: &str| {
            json!({ "timestamp": "2026-10-11T19:50:53.000Z", "type": "response_item",
            "payload": { "type": "message", "role": role, "content": [{ "type": ty, "text": text }] } })
        };
        let text = jl(&[
            json!({ "type": "session_meta", "payload": { "id": "c1", "cwd": "/w" } }),
            msg("developer", "input_text", "You are Codex"),
            msg("user", "input_text", "# AGENTS.md instructions for /w"),
            msg(
                "user",
                "input_text",
                "<environment_context>x</environment_context>",
            ),
            msg("user", "input_text", "fix the bug"),
            msg("assistant", "output_text", "Looking at it."),
            json!({ "type": "response_item", "payload": { "type": "function_call", "name": "exec_command", "call_id": "call_1",
                    "arguments": "{\"cmd\":\"rg -n bug src\",\"workdir\":\"/w\"}" } }),
            json!({ "type": "response_item", "payload": { "type": "function_call_output", "call_id": "call_1", "output": "SECRET OUTPUT" } }),
            json!({ "type": "response_item", "payload": { "type": "custom_tool_call", "name": "apply_patch", "call_id": "call_2",
                    "input": "*** Begin Patch\n*** Update File: src/a.rs\n@@\n-secret old\n+secret new\n*** End Patch" } }),
            json!({ "type": "response_item", "payload": { "type": "reasoning", "summary": [{ "type": "summary_text", "text": "thinking" }] } }),
            msg("assistant", "output_text", "Fixed."),
            json!({ "type": "event_msg", "payload": { "type": "task_complete" } }),
        ]);
        let p = parse(Harness::Codex, text.as_bytes(), 0);
        assert_eq!(
            shape(&p.entries),
            vec![
                ("user", Kind::User, "fix the bug".into(), None),
                (
                    "assistant",
                    Kind::Assistant,
                    "Looking at it.".into(),
                    Some(false)
                ),
                (
                    "assistant",
                    Kind::ToolCall,
                    "exec_command: rg -n bug src".into(),
                    None
                ),
                (
                    "assistant",
                    Kind::ToolCall,
                    "apply_patch: src/a.rs".into(),
                    None
                ),
                ("assistant", Kind::Assistant, "Fixed.".into(), Some(true)),
            ]
        );
        assert_eq!(p.entries[2].id.as_deref(), Some("call_1"));
        let all = serde_json::to_string(&p.entries).unwrap();
        assert!(!all.contains("SECRET") && !all.contains("secret") && !all.contains("thinking"));
        // The final reply can still be followed by a tool call: not clean until a new turn.
        assert!(p.clean_at_end < p.end);
    }

    #[test]
    fn pi_messages_and_tool_calls() {
        let text = jl(&[
            json!({ "type": "session", "id": "p1", "cwd": "/w" }),
            json!({ "type": "message", "timestamp": "2026-10-11T19:50:44.044Z", "message": { "role": "user", "content": [{ "type": "text", "text": "hi" }] } }),
            json!({ "type": "message", "message": { "role": "assistant", "timestamp": 1791748244044_i64, "stopReason": "toolUse",
                    "content": [{ "type": "text", "text": "checking" }, { "type": "toolCall", "id": "tc1", "name": "read", "arguments": { "path": "src/main.rs" } }] } }),
            json!({ "type": "message", "message": { "role": "toolResult", "content": [{ "type": "text", "text": "SECRET OUTPUT" }] } }),
            json!({ "type": "message", "message": { "role": "assistant", "content": [{ "type": "text", "text": "done" }], "stopReason": "stop" } }),
        ]);
        let p = parse(Harness::Pi, text.as_bytes(), 0);
        assert_eq!(
            shape(&p.entries),
            vec![
                ("user", Kind::User, "hi".into(), None),
                ("assistant", Kind::Assistant, "checking".into(), Some(false)),
                (
                    "assistant",
                    Kind::ToolCall,
                    "read: src/main.rs".into(),
                    None
                ),
                ("assistant", Kind::Assistant, "done".into(), Some(true)),
            ]
        );
        assert_eq!(p.entries[0].ts.as_deref(), Some("2026-10-11T19:50:44.044Z"));
        assert_eq!(p.entries[1].ts.as_deref(), Some("2026-10-11T19:50:44.044Z"));
        // Two entries from one line: same offset, sub-indices 0 and 1.
        assert_eq!(p.entries[1].pos.off, p.entries[2].pos.off);
        assert_eq!((p.entries[1].pos.sub, p.entries[2].pos.sub), (0, 1));
        assert!(
            !serde_json::to_string(&p.entries)
                .unwrap()
                .contains("SECRET")
        );
    }

    #[test]
    fn tool_summaries_are_one_short_argument() {
        assert_eq!(
            tool_summary("Read", &json!({ "file_path": "/w/a.rs", "limit": 5 })),
            "/w/a.rs"
        );
        assert_eq!(
            tool_summary(
                "Write",
                &json!({ "file_path": "/w/a.rs", "content": "secret" })
            ),
            "/w/a.rs"
        );
        assert_eq!(
            tool_summary(
                "Edit",
                &json!({ "file_path": "f", "old_string": "a", "new_string": "b" })
            ),
            "f"
        );
        assert_eq!(
            tool_summary("Grep", &json!({ "pattern": "fn main", "path": "src" })),
            "fn main"
        );
        assert_eq!(
            tool_summary(
                "Task",
                &json!({ "description": "Explore", "prompt": "long" })
            ),
            "Explore"
        );
        assert_eq!(
            tool_summary("TodoWrite", &json!({ "todos": [1, 2, 3] })),
            "3 todos"
        );
        assert_eq!(
            tool_summary("shell", &json!({ "command": ["bash", "-lc", "ls -la"] })),
            "ls -la"
        );
        assert_eq!(
            tool_summary("mcp__x__y", &json!({ "content": "secret", "query": "q" })),
            "q"
        );
        assert_eq!(
            tool_summary("mcp__x__y", &json!({ "content": "secret" })),
            ""
        );
        let long = "x".repeat(300);
        let s = tool_summary("Bash", &json!({ "command": long }));
        assert_eq!(s.chars().count(), MAX_SUMMARY);
        assert!(s.ends_with('…'));
        assert_eq!(
            custom_summary("exec", "\n  const a = 1;\nsecret()"),
            "const a = 1;"
        );
    }

    #[test]
    fn a_half_written_last_line_waits() {
        let full = jl(&[user(json!("one")), user(json!("two"))]);
        let cut = full.len() - 10;
        let p = parse(Harness::Claude, &full.as_bytes()[..cut], 0);
        assert_eq!(p.entries.len(), 1);
        let first_line = full.find('\n').unwrap() as u64 + 1;
        assert_eq!(p.end, first_line);
    }

    // --- reading files -------------------------------------------------------

    struct File_ {
        _dir: tempfile::TempDir,
        path: std::path::PathBuf,
    }

    impl File_ {
        fn new(text: &str) -> Self {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("s.jsonl");
            std::fs::write(&path, text).unwrap();
            File_ { _dir: dir, path }
        }
        fn append(&self, text: &str) {
            use std::io::Write;
            let mut f = std::fs::OpenOptions::new()
                .append(true)
                .open(&self.path)
                .unwrap();
            f.write_all(text.as_bytes()).unwrap();
        }
        fn len(&self) -> u64 {
            std::fs::metadata(&self.path).unwrap().len()
        }
    }

    fn texts(es: &[Entry]) -> Vec<&str> {
        es.iter().map(|e| e.text.as_str()).collect()
    }
    fn cur(s: &str) -> Cursor {
        Cursor::parse(s).unwrap()
    }
    /// A page's poll cursor.
    fn pc(p: &Page) -> Cursor {
        cur(p.cursor.as_deref().expect("a page cursor"))
    }
    fn prompts(range: std::ops::Range<usize>) -> String {
        jl(&range
            .map(|i| user(json!(format!("p{i}"))))
            .collect::<Vec<_>>())
    }

    #[test]
    fn latest_then_before_pages_back_and_after_pages_forward() {
        let f = File_::new(&prompts(0..10));
        let page = latest(&f.path, Harness::Claude, 3).unwrap();
        assert_eq!(texts(&page.entries), ["p7", "p8", "p9"]);
        assert!(page.has_more_before && !page.has_more_after);

        let older = before(&f.path, Harness::Claude, &cur(&page.entries[0].cursor), 3).unwrap();
        assert_eq!(texts(&older.entries), ["p4", "p5", "p6"]);
        assert!(older.has_more_before && older.has_more_after);
        let oldest = before(&f.path, Harness::Claude, &cur(&older.entries[0].cursor), 10).unwrap();
        assert_eq!(texts(&oldest.entries), ["p0", "p1", "p2", "p3"]);
        assert!(!oldest.has_more_before);

        // Forward from the oldest entry, a page at a time.
        let c = cur(&oldest.entries[0].cursor);
        let fwd = after(&f.path, Harness::Claude, &c, Some(4)).unwrap();
        assert_eq!(texts(&fwd.entries), ["p1", "p2", "p3", "p4"]);
        assert!(fwd.has_more_after);
        let rest = after(&f.path, Harness::Claude, &pc(&fwd), None).unwrap();
        assert_eq!(texts(&rest.entries), ["p5", "p6", "p7", "p8", "p9"]);
        assert!(!rest.has_more_after);
        assert!(rest.updated.is_empty());
        // Entry cursors are stable across calls.
        assert_eq!(rest.entries[4].cursor, page.entries[2].cursor);
    }

    #[test]
    fn polling_after_reads_only_new_bytes() {
        let f = File_::new(&prompts(0..50));
        let page = latest(&f.path, Harness::Claude, 100).unwrap();
        assert_eq!(page.entries.len(), 50);
        let idle = after(&f.path, Harness::Claude, &pc(&page), None).unwrap();
        assert!(idle.entries.is_empty());
        assert_eq!(idle.bytes_read, 0);
        assert_eq!(
            idle.cursor, page.cursor,
            "nothing new: the same cursor back"
        );

        let before_len = f.len();
        let added = prompts(50..52);
        f.append(&added);
        let next = after(&f.path, Harness::Claude, &pc(&idle), None).unwrap();
        assert_eq!(texts(&next.entries), ["p50", "p51"]);
        assert_eq!(next.bytes_read, f.len() - before_len);
        assert_eq!(next.bytes_read, added.len() as u64);
    }

    #[test]
    fn a_half_written_line_is_returned_once_complete() {
        let f = File_::new(&prompts(0..2));
        let page = latest(&f.path, Harness::Claude, 10).unwrap();
        let line = format!("{}\n", user(json!("late")));
        let (a, b) = line.split_at(line.len() / 2);
        f.append(a);
        let mid = after(&f.path, Harness::Claude, &pc(&page), None).unwrap();
        assert!(mid.entries.is_empty());
        f.append(b);
        let done = after(&f.path, Harness::Claude, &pc(&mid), None).unwrap();
        assert_eq!(texts(&done.entries), ["late"]);
        // A latest read with the half line pending does not show it either.
        let f2 = File_::new(&(prompts(0..1) + a));
        assert_eq!(
            texts(&latest(&f2.path, Harness::Claude, 10).unwrap().entries),
            ["p0"]
        );
    }

    #[test]
    fn a_question_goes_from_pending_to_answered_in_updated() {
        let f = File_::new(&jl(&[
            user(json!("deploy?")),
            asst(
                json!([{ "type": "tool_use", "id": "tq", "name": "AskUserQuestion", "input": question_input() }]),
                "m1",
                "tool_use",
            ),
        ]));
        let page = latest(&f.path, Harness::Claude, 10).unwrap();
        assert_eq!(page.entries[1].status, Some("pending"));
        let q_cursor = page.entries[1].cursor.clone();
        // Nothing new yet: the open question is re-read, but not reported again.
        let idle = after(&f.path, Harness::Claude, &pc(&page), None).unwrap();
        assert!(idle.entries.is_empty() && idle.updated.is_empty());

        f.append(&jl(&[
            json!({ "type": "user", "message": { "role": "user", "content": [{ "type": "tool_result", "tool_use_id": "tq", "content": "ok" }] },
                    "toolUseResult": { "answers": { "Merge it?": "No" } } }),
            asst(json!([{ "type": "text", "text": "Holding off." }]), "m2", "end_turn"),
        ]));
        let next = after(&f.path, Harness::Claude, &pc(&idle), None).unwrap();
        assert_eq!(texts(&next.entries), ["Holding off."]);
        assert_eq!(next.updated.len(), 1);
        let q = &next.updated[0];
        assert_eq!(
            (q.cursor.as_str(), q.status),
            (q_cursor.as_str(), Some("answered"))
        );
        assert_eq!(
            q.answer,
            Some(json!({ "answers": ["No"], "picked": [[1]] }))
        );
        // Settled: the next poll starts at the end again.
        let calm = after(&f.path, Harness::Claude, &pc(&next), None).unwrap();
        assert!(calm.updated.is_empty() && calm.entries.is_empty());
    }

    #[test]
    fn streamed_text_glued_after_a_poll_comes_back_in_updated() {
        let f = File_::new(&jl(&[
            user(json!("q")),
            asst(
                json!([{ "type": "text", "text": "part one" }]),
                "same",
                "end_turn",
            ),
        ]));
        let page = latest(&f.path, Harness::Claude, 10).unwrap();
        f.append(&jl(&[asst(
            json!([{ "type": "text", "text": "part two" }]),
            "same",
            "end_turn",
        )]));
        let next = after(&f.path, Harness::Claude, &pc(&page), None).unwrap();
        assert!(next.entries.is_empty());
        assert_eq!(texts(&next.updated), ["part one\n\npart two"]);
        assert_eq!(next.updated[0].cursor, page.entries[1].cursor);
    }

    #[test]
    fn codex_narration_turned_non_final_comes_back_in_updated() {
        let msg = json!({ "type": "response_item", "payload": { "type": "message", "role": "assistant", "content": [{ "type": "output_text", "text": "Looking." }] } });
        let f = File_::new(&jl(&[msg]));
        let page = latest(&f.path, Harness::Codex, 10).unwrap();
        assert_eq!(page.entries[0].final_, Some(true));
        f.append(&jl(&[json!({ "type": "response_item", "payload": { "type": "function_call", "name": "exec_command", "call_id": "c1", "arguments": "{\"cmd\":\"ls\"}" } })]));
        let next = after(&f.path, Harness::Codex, &pc(&page), None).unwrap();
        assert_eq!(texts(&next.entries), ["exec_command: ls"]);
        assert_eq!(next.updated[0].final_, Some(false));
    }

    #[test]
    fn queued_prompts_are_resent_until_dequeued() {
        let q = |op: &str, c: Option<&str>| {
            let mut v = json!({ "type": "queue-operation", "operation": op });
            if let Some(c) = c {
                v["content"] = json!(c);
            }
            v
        };
        let f = File_::new(&jl(&[user(json!("start")), q("enqueue", Some("later"))]));
        let page = latest(&f.path, Harness::Claude, 10).unwrap();
        assert_eq!(texts(&page.entries), ["start", "later"]);
        assert!(page.entries[1].queued && page.entries[1].cursor.starts_with("q2."));
        // The poll cursor is the newest real entry, never the queued one.
        assert_eq!(pc(&page).pos, cur(&page.entries[0].cursor).pos);
        assert!(Cursor::parse(&page.entries[1].cursor).is_err());

        let again = after(&f.path, Harness::Claude, &pc(&page), None).unwrap();
        assert_eq!(texts(&again.entries), ["later"]);
        f.append(&jl(&[q("dequeue", None), user(json!("later"))]));
        let taken = after(&f.path, Harness::Claude, &pc(&again), None).unwrap();
        assert_eq!(
            taken
                .entries
                .iter()
                .map(|e| (e.text.as_str(), e.queued))
                .collect::<Vec<_>>(),
            [("later", false)]
        );
    }

    #[test]
    fn invalid_cursors_are_refused() {
        let f = File_::new(&prompts(0..5));
        let page = latest(&f.path, Harness::Claude, 10).unwrap();
        let c = pc(&page);
        // Rewritten: same length, different bytes.
        std::fs::write(&f.path, prompts(0..5).replace("p4", "x4")).unwrap();
        assert!(matches!(
            after(&f.path, Harness::Claude, &c, None),
            Err(ReadError::CursorInvalid(_))
        ));
        // Shrunk.
        std::fs::write(&f.path, prompts(0..2)).unwrap();
        assert!(matches!(
            after(&f.path, Harness::Claude, &c, None),
            Err(ReadError::CursorInvalid(_))
        ));
        let entry = cur(&page.entries[4].cursor);
        assert!(matches!(
            before(&f.path, Harness::Claude, &entry, 5),
            Err(ReadError::CursorInvalid(_))
        ));
        // Not a line start / a fingerprint of another line.
        std::fs::write(&f.path, prompts(0..5)).unwrap();
        let shifted = Cursor {
            pos: Pos {
                off: entry.pos.off + 1,
                sub: 0,
            },
            ..entry.clone()
        };
        assert!(matches!(
            after(&f.path, Harness::Claude, &shifted, None),
            Err(ReadError::CursorInvalid(_))
        ));
        let other = Cursor {
            fp: entry.fp ^ 1,
            ..entry
        };
        assert!(matches!(
            before(&f.path, Harness::Claude, &other, 5),
            Err(ReadError::CursorInvalid(_))
        ));
        // A hintless entry cursor still works for --after (it reads from its line).
        let fwd = after(
            &f.path,
            Harness::Claude,
            &cur(&page.entries[2].cursor),
            None,
        )
        .unwrap();
        assert_eq!(texts(&fwd.entries), ["p3", "p4"]);
    }

    #[test]
    fn before_resolves_a_question_answered_after_the_window() {
        // A question and a parallel tool call in one message; both results come after.
        let f = File_::new(&jl(&[
            user(json!("go")),
            asst(
                json!([{ "type": "tool_use", "id": "tq", "name": "AskUserQuestion", "input": question_input() }]),
                "m1",
                "tool_use",
            ),
            asst(
                json!([{ "type": "tool_use", "id": "tb", "name": "Bash", "input": { "command": "ls" } }]),
                "m1",
                "tool_use",
            ),
            user(json!([{ "type": "tool_result", "tool_use_id": "tb", "content": "out" }])),
            user(
                json!([{ "type": "tool_result", "tool_use_id": "tq", "content": "no", "is_error": true }]),
            ),
            asst(
                json!([{ "type": "text", "text": "OK, stopping." }]),
                "m2",
                "end_turn",
            ),
        ]));
        let all = latest(&f.path, Harness::Claude, 10).unwrap();
        assert_eq!(texts(&all.entries)[2], "Bash: ls");
        assert_eq!(all.entries[1].status, Some("declined"));
        // The window ends at the Bash call, before the question's result: found past it.
        let w = before(&f.path, Harness::Claude, &cur(&all.entries[2].cursor), 5).unwrap();
        assert_eq!(w.entries[1].kind, Kind::Question);
        assert_eq!(w.entries[1].status, Some("declined"));
        assert_eq!(w.entries[1].answer, Some(json!({ "declined": true })));
    }

    #[test]
    fn the_tail_grows_until_it_holds_the_limit() {
        // Big tool results between prompts: a 2 MiB tail holds only the last prompt.
        let blob = user(
            json!([{ "type": "tool_result", "tool_use_id": "t", "content": "x".repeat(1 << 20) }]),
        );
        let f = File_::new(&jl(&[
            user(json!("first")),
            blob.clone(),
            blob.clone(),
            blob,
            user(json!("latest")),
        ]));
        let one = latest(&f.path, Harness::Claude, 1).unwrap();
        assert_eq!(texts(&one.entries), ["latest"]);
        assert!(one.has_more_before);
        let all = latest(&f.path, Harness::Claude, 60).unwrap();
        assert_eq!(texts(&all.entries), ["first", "latest"]);
        assert!(!all.has_more_before);
    }

    #[test]
    fn an_empty_transcript_has_no_cursor_until_it_has_entries() {
        let f = File_::new(&jl(&[
            json!({ "type": "permission-mode", "permissionMode": "default" }),
        ]));
        let page = latest(&f.path, Harness::Claude, 10).unwrap();
        assert!(page.entries.is_empty());
        assert_eq!(page.cursor, None);
        f.append(&prompts(0..1));
        let page = latest(&f.path, Harness::Claude, 10).unwrap();
        assert_eq!(texts(&page.entries), ["p0"]);
        assert!(page.cursor.is_some());
        // The empty-transcript cursor still reads from the start.
        let from_empty = after(&f.path, Harness::Claude, &Cursor::empty(), None).unwrap();
        assert_eq!(texts(&from_empty.entries), ["p0"]);
        // Nothing older than the oldest entry: no cursor either.
        let none = before(&f.path, Harness::Claude, &cur(&page.entries[0].cursor), 10).unwrap();
        assert_eq!((none.entries.len(), none.cursor), (0, None));
    }

    #[test]
    fn cursors_are_short_printable_and_never_look_like_flags() {
        let f = File_::new(&prompts(0..3));
        let page = latest(&f.path, Harness::Claude, 10).unwrap();
        let all: Vec<String> = page
            .entries
            .iter()
            .map(|e| e.cursor.clone())
            .chain(page.cursor.clone())
            .collect();
        let worst = |queue: usize| {
            Cursor {
                pos: Pos {
                    off: u64::MAX,
                    sub: i64::MAX,
                },
                fp: u32::MAX,
                fid: u32::MAX,
                hint: Some(Hint {
                    end: u64::MAX,
                    rescan: u64::MAX,
                    end_fp: u32::MAX,
                    queue: Some(vec![(u64::MAX, u32::MAX); queue]),
                    paging: true,
                }),
            }
            .encode()
        };
        // A queue too long to carry: the cursor says so ("!") instead of growing past the cap.
        let overflow = worst(100);
        assert!(overflow.ends_with(".!.p"), "{overflow}");
        for c in all.iter().chain([&worst(0), &worst(3), &overflow]) {
            assert!(c.len() <= MAX_CURSOR_LEN && !c.starts_with('-'), "{c}");
            assert!(c.bytes().all(|b| (0x21..0x7f).contains(&b)), "{c}");
        }
    }

    #[test]
    fn cursors_round_trip_and_reject_garbage() {
        let c = Cursor {
            pos: Pos {
                off: 0x1a2b,
                sub: 2,
            },
            fp: 0xdead_beef,
            fid: 0x1234,
            hint: Some(Hint {
                end: 0x4000,
                rescan: 0x3000,
                end_fp: 7,
                queue: Some(vec![(0x3100, 9), (0x3200, 0xffff_ffff)]),
                paging: true,
            }),
        };
        assert_eq!(Cursor::parse(&c.encode()), Ok(c.clone()));
        let bare = Cursor {
            hint: None,
            ..c.clone()
        };
        assert_eq!(bare.encode(), "m2.1a2b.2.deadbeef.00001234");
        assert_eq!(Cursor::parse(&bare.encode()), Ok(bare));
        let mut no_queue = c.clone();
        no_queue.hint.as_mut().unwrap().queue = Some(Vec::new());
        assert_eq!(Cursor::parse(&no_queue.encode()), Ok(no_queue));
        let mut delivered = c.clone();
        delivered.hint.as_mut().unwrap().paging = false;
        assert!(delivered.encode().ends_with(".a"));
        assert_eq!(Cursor::parse(&delivered.encode()), Ok(delivered));
        let mut overflow = c;
        overflow.hint.as_mut().unwrap().queue = None;
        assert_eq!(Cursor::parse(&overflow.encode()), Ok(overflow));
        assert_eq!(
            Cursor::parse(&Cursor::empty().encode()),
            Ok(Cursor::empty())
        );
        for bad in [
            "",
            "m2",
            "m1.1.0.0",
            "m2.zz.0.0.0",
            "m2.1.-2.0.0",
            "m2.1.0.0.0.1.2.0.",
            "m2.1.0.0.0.4.2.0.x",
            "m2.1.0.0.0.1.2.0..z",
            "q2.1.0.0",
        ] {
            assert!(Cursor::parse(bad).is_err(), "{bad}");
        }
    }

    // --- round 2 regressions ---------------------------------------------------

    #[test]
    fn text_on_both_sides_of_a_tool_call_is_one_entry_like_the_web() {
        // Text A → tool use → text B, all one API message: the web shows "A\n\nB".
        let lines = [
            user(json!("go")),
            asst(json!([{ "type": "text", "text": "A" }]), "m1", "tool_use"),
            asst(
                json!([{ "type": "tool_use", "id": "t1", "name": "Bash", "input": { "command": "ls" } }]),
                "m1",
                "tool_use",
            ),
        ];
        let f = File_::new(&jl(&lines));
        let page = latest(&f.path, Harness::Claude, 10).unwrap();
        assert_eq!(texts(&page.entries), ["go", "A", "Bash: ls"]);
        // B arrives after a poll: A's entry comes back, glued, in `updated`.
        f.append(&jl(&[asst(
            json!([{ "type": "text", "text": "B" }]),
            "m1",
            "end_turn",
        )]));
        let next = after(&f.path, Harness::Claude, &pc(&page), None).unwrap();
        assert!(next.entries.is_empty(), "{:?}", texts(&next.entries));
        assert_eq!(texts(&next.updated), ["A\n\nB"]);
        assert_eq!(next.updated[0].cursor, page.entries[1].cursor);
        assert_eq!(next.updated[0].final_, Some(true));
        // The whole file parsed at once agrees.
        let whole = latest(&f.path, Harness::Claude, 10).unwrap();
        assert_eq!(texts(&whole.entries), ["go", "A\n\nB", "Bash: ls"]);
        // The tool result closes the message: a later poll starts at the end again.
        f.append(&jl(&[user(
            json!([{ "type": "tool_result", "tool_use_id": "t1", "content": "x" }]),
        )]));
        let calm = after(&f.path, Harness::Claude, &pc(&next), None).unwrap();
        let h = Cursor::parse(calm.cursor.as_deref().unwrap())
            .unwrap()
            .hint
            .unwrap();
        assert_eq!(h.rescan, h.end);
    }

    fn qop(op: &str, content: Option<&str>) -> Value {
        let mut v = json!({ "type": "queue-operation", "operation": op });
        if let Some(c) = content {
            v["content"] = json!(c);
        }
        v
    }

    #[test]
    fn hidden_queue_items_keep_their_place_across_polls() {
        // A task notification (never shown) is queued first, a typed prompt after it.
        let f = File_::new(&jl(&[
            user(json!("start")),
            qop(
                "enqueue",
                Some("<task-notification><summary>done</summary></task-notification>"),
            ),
            qop("enqueue", Some("typed later")),
        ]));
        let page = latest(&f.path, Harness::Claude, 10).unwrap();
        assert_eq!(texts(&page.entries), ["start", "typed later"]);
        // The poll cursor carries both items, the hidden one included.
        let h = pc(&page).hint.unwrap();
        assert_eq!(h.queue.as_ref().map(Vec::len), Some(2));
        // One dequeue takes the notification (FIFO): the prompt is still queued.
        f.append(&jl(&[qop("dequeue", None)]));
        let next = after(&f.path, Harness::Claude, &pc(&page), None).unwrap();
        assert_eq!(
            next.entries
                .iter()
                .map(|e| (e.text.as_str(), e.queued))
                .collect::<Vec<_>>(),
            [("typed later", true)]
        );
        // The next takes the prompt.
        f.append(&jl(&[qop("dequeue", None), user(json!("typed later"))]));
        let taken = after(&f.path, Harness::Claude, &pc(&next), None).unwrap();
        assert_eq!(
            taken
                .entries
                .iter()
                .map(|e| (e.text.as_str(), e.queued))
                .collect::<Vec<_>>(),
            [("typed later", false)]
        );
    }

    #[test]
    fn open_state_too_far_back_is_refused_not_lost() {
        // A pending question, then more than the rescan bound of lines nobody reads.
        let noise = json!({ "type": "api-request", "blob": "x".repeat(64 << 10) });
        let mut lines = vec![
            user(json!("go")),
            asst(
                json!([{ "type": "tool_use", "id": "tq", "name": "AskUserQuestion", "input": question_input() }]),
                "m1",
                "tool_use",
            ),
        ];
        lines.extend(std::iter::repeat_n(noise, 40));
        let f = File_::new(&jl(&lines));
        let page = latest(&f.path, Harness::Claude, 10).unwrap();
        assert_eq!(page.entries[1].status, Some("pending"));
        assert!(matches!(
            after(&f.path, Harness::Claude, &pc(&page), None),
            Err(ReadError::CursorInvalid(_))
        ));
        // Reloading still shows the question, still pending.
        let again = latest(&f.path, Harness::Claude, 10).unwrap();
        assert_eq!(again.entries[1].status, Some("pending"));
    }

    #[test]
    fn rewrites_inside_the_anchor_line_and_replaced_files_are_caught() {
        // A long anchor line, edited near its start with the length kept.
        let long = format!("{}{}", "start-", "y".repeat(4000));
        let f = File_::new(&jl(&[user(json!("one")), user(json!(long))]));
        let page = latest(&f.path, Harness::Claude, 10).unwrap();
        let edited = std::fs::read_to_string(&f.path)
            .unwrap()
            .replacen("start-", "START-", 1);
        // Rewritten in place (same inode).
        {
            use std::io::{Seek, SeekFrom, Write};
            let mut w = std::fs::OpenOptions::new()
                .write(true)
                .open(&f.path)
                .unwrap();
            w.seek(SeekFrom::Start(0)).unwrap();
            w.write_all(edited.as_bytes()).unwrap();
        }
        for c in [pc(&page), cur(&page.entries[1].cursor)] {
            assert!(
                matches!(
                    after(&f.path, Harness::Claude, &c, None),
                    Err(ReadError::CursorInvalid(_))
                ),
                "{c:?}"
            );
        }
        // Same bytes, but another file (an atomic replace): another identity.
        let f2 = File_::new(&jl(&[user(json!("one")), user(json!("two"))]));
        let p2 = latest(&f2.path, Harness::Claude, 10).unwrap();
        let copy = f2.path.with_extension("tmp");
        std::fs::copy(&f2.path, &copy).unwrap();
        std::fs::rename(&copy, &f2.path).unwrap();
        assert!(matches!(
            after(&f2.path, Harness::Claude, &pc(&p2), None),
            Err(ReadError::CursorInvalid(_))
        ));
    }

    #[test]
    fn a_limited_page_keeps_the_queue_and_reports_an_update_once() {
        let f = File_::new(&jl(&[
            user(json!("go")),
            asst(
                json!([{ "type": "tool_use", "id": "tq", "name": "AskUserQuestion", "input": question_input() }]),
                "m1",
                "tool_use",
            ),
        ]));
        let page = latest(&f.path, Harness::Claude, 10).unwrap();
        f.append(&jl(&[
            json!({ "type": "user", "message": { "role": "user", "content": [{ "type": "tool_result", "tool_use_id": "tq", "content": "ok" }] },
                    "toolUseResult": { "answers": { "Merge it?": "Yes" } } }),
            user(json!("p1")),
            user(json!("p2")),
            qop("enqueue", Some("waiting")),
        ]));
        let one = after(&f.path, Harness::Claude, &pc(&page), Some(1)).unwrap();
        assert!(one.has_more_after);
        assert_eq!(
            texts(&one.entries),
            ["p1", "waiting"],
            "the queue comes along"
        );
        assert_eq!(one.updated.len(), 1, "the answer, once");
        let two = after(&f.path, Harness::Claude, &pc(&one), Some(1)).unwrap();
        assert_eq!(texts(&two.entries), ["p2", "waiting"]);
        assert!(two.updated.is_empty(), "not reported again");
        assert!(!two.has_more_after);
    }

    // --- round 3 regressions ---------------------------------------------------

    #[test]
    fn paging_back_keeps_text_glued_after_a_tool_call() {
        // A → tool call → B under one message id: B lies after the tool call, beyond a
        // `--before` window that ends there.
        let f = File_::new(&jl(&[
            user(json!("go")),
            asst(json!([{ "type": "text", "text": "A" }]), "m1", "tool_use"),
            asst(
                json!([{ "type": "tool_use", "id": "t1", "name": "Bash", "input": { "command": "ls" } }]),
                "m1",
                "tool_use",
            ),
            asst(json!([{ "type": "text", "text": "B" }]), "m1", "tool_use"),
            user(json!([{ "type": "tool_result", "tool_use_id": "t1", "content": "x" }])),
        ]));
        let newest = latest(&f.path, Harness::Claude, 1).unwrap();
        assert_eq!(texts(&newest.entries), ["Bash: ls"]);
        let older = before(&f.path, Harness::Claude, &cur(&newest.entries[0].cursor), 5).unwrap();
        assert_eq!(texts(&older.entries), ["go", "A\n\nB"]);
    }

    #[test]
    fn a_question_overtaken_by_a_new_prompt_is_abandoned() {
        let ask = |id: &str, msg: &str| {
            asst(
                json!([{ "type": "tool_use", "id": id, "name": "AskUserQuestion", "input": question_input() }]),
                msg,
                "tool_use",
            )
        };
        let f = File_::new(&jl(&[user(json!("go")), ask("old", "m1")]));
        let page = latest(&f.path, Harness::Claude, 10).unwrap();
        assert_eq!(page.entries[1].status, Some("pending"));
        // The session restarted on it; a new prompt, the same question asked again.
        f.append(&jl(&[user(json!("go again")), ask("new", "m2")]));
        let next = after(&f.path, Harness::Claude, &pc(&page), None).unwrap();
        assert_eq!(
            next.updated
                .iter()
                .map(|e| (e.id.as_deref(), e.status))
                .collect::<Vec<_>>(),
            [(Some("old"), Some("abandoned"))]
        );
        assert_eq!(next.updated[0].answer, Some(Value::Null));
        let all = latest(&f.path, Harness::Claude, 10).unwrap();
        let qs: Vec<_> = all
            .entries
            .iter()
            .filter(|e| e.kind == Kind::Question)
            .map(|e| (e.id.as_deref(), e.status))
            .collect();
        assert_eq!(
            qs,
            [
                (Some("old"), Some("abandoned")),
                (Some("new"), Some("pending"))
            ]
        );
        // Paging back to the old one finds it abandoned too (settled past the window).
        let back = before(&f.path, Harness::Claude, &cur(&all.entries[2].cursor), 5).unwrap();
        assert_eq!(back.entries.last().unwrap().status, Some("abandoned"));
    }

    #[test]
    fn a_limited_page_through_a_closed_backlog_keeps_going() {
        let f = File_::new(&prompts(0..1));
        let page = latest(&f.path, Harness::Claude, 10).unwrap();
        // Two prompts, then more than the rescan bound of lines nobody reads.
        let noise = json!({ "type": "api-request", "blob": "x".repeat(64 << 10) });
        let mut lines = vec![user(json!("p1")), user(json!("p2"))];
        lines.extend(std::iter::repeat_n(noise, 40));
        f.append(&jl(&lines));
        let one = after(&f.path, Harness::Claude, &pc(&page), Some(1)).unwrap();
        assert_eq!(texts(&one.entries), ["p1"]);
        assert!(one.has_more_after);
        let two = after(&f.path, Harness::Claude, &pc(&one), Some(1)).unwrap();
        assert_eq!(texts(&two.entries), ["p2"]);
        assert!(!two.has_more_after);
        // Delivered and nothing open: the next poll reads only new bytes.
        let idle = after(&f.path, Harness::Claude, &pc(&two), None).unwrap();
        assert_eq!(idle.bytes_read, 0);
    }
}
