//! Session stacks (docs/architecture.md → "Session stacks").
//!
//! A stack is a set of Claude Code sessions sharing one context layer: a markdown file, the
//! **StackBrief**, one per stack (`<dir>/<stack_id>.md`). The file *is* the state — there is no
//! index. It is written once with a model call (Summary / Resources) when the stack is created;
//! after that only scripts touch it (membership: [`sync`], add, remove) and humans (or any
//! session) edit it by hand. Format conventions are the session briefs' ([`crate::core::brief`]):
//! the same frontmatter encoding, atomic writes, tolerant parsing, human lines never dropped.
//!
//! A stack lives on the host where it was created (its *home*, frontmatter `host`); its members
//! may run on any host (`members[].host`). A sibling on another host gets the shared context
//! through `fleet -H <home> stack …` instead of a path ([`remote_context_line`]), and a
//! membership sync judges a member only against the rows of its own host ([`Observed`]).
//!
//! Nothing in here prints.

use std::collections::{HashMap, HashSet};
use std::hash::{BuildHasher, Hasher};
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

use crate::core::brief::{self, Extra};
use crate::core::discovery::{self, Session};
use crate::error::{Error, Result};

/// Exit code: two stacks (or sessions) match the query on the same rung.
pub const EXIT_AMBIGUOUS: i32 = 2;
/// Exit code: nothing matches / a conflict (the file changed since it was read).
pub const EXIT_NOTHING: i32 = 3;
/// The longest human edit accepted (the web API's limit too).
pub const MAX_EDIT_BYTES: usize = 64 * 1024;
/// The creation call's ceiling (`claude -p` start-up included).
pub const GEN_TIMEOUT: Duration = Duration::from_secs(120);
/// How much of the source session's conversation goes into the creation prompt.
pub const TAIL_CHARS: usize = 6000;
const SUMMARY_CAP: usize = 1000;
const FIRST_PROMPT_CAP: usize = 160;
const LABEL_CAP: usize = 80;
const HEADER_MARK: &str = "Shared context for the session stack";

/// Frontmatter keys in the order they are written; unknown keys follow, as found.
const META_ORDER: [&str; 9] = [
    "stack",
    "label",
    "host",
    "cwd",
    "created",
    "updated",
    "generatedAt",
    "editedAt",
    "members",
];
/// Keys a human edit never changes: the stored file's win.
const MACHINE_KEYS: [&str; 8] = [
    "stack",
    "host",
    "cwd",
    "created",
    "updated",
    "generatedAt",
    "editedAt",
    "members",
];

// ------------------------------------------------------------------ storage

/// `$FLEET_STACKS_DIR`, else `${XDG_STATE_HOME:-~/.local/state}/fleet/stacks`.
pub fn stacks_dir() -> Result<PathBuf> {
    let env = |k: &str| std::env::var(k).ok().filter(|v| !v.is_empty());
    let expand = |p: &str| PathBuf::from(crate::core::tools::expand_tilde(p));
    let dir = if let Some(d) = env("FLEET_STACKS_DIR") {
        let p = expand(&d);
        if p.is_absolute() {
            p
        } else {
            std::env::current_dir()?.join(p)
        }
    } else {
        let base = match env("XDG_STATE_HOME") {
            Some(x) => expand(&x),
            None => dirs::home_dir()
                .ok_or_else(|| Error::Other("no home directory".into()))?
                .join(".local")
                .join("state"),
        };
        base.join("fleet").join("stacks")
    };
    if !dir.is_absolute() {
        return Err(Error::Other(format!(
            "stacks dir must be an absolute path: {}",
            dir.display()
        )));
    }
    Ok(dir)
}

/// `st-` + 8 lowercase hex characters.
pub fn is_stack_id(id: &str) -> bool {
    id.strip_prefix("st-").is_some_and(|h| {
        h.len() == 8
            && h.bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    })
}

/// A fresh random id that no file in `dir` has yet.
pub fn new_id(dir: &Path) -> String {
    loop {
        let mut h = std::collections::hash_map::RandomState::new().build_hasher();
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        h.write_u128(nanos);
        h.write_u32(std::process::id());
        let id = format!("st-{:08x}", h.finish() as u32);
        if !dir.join(format!("{id}.md")).exists() {
            return id;
        }
    }
}

pub fn file(dir: &Path, id: &str) -> Result<PathBuf> {
    if !is_stack_id(id) {
        return Err(Error::Other(format!("not a stack id: {id}")));
    }
    Ok(dir.join(format!("{id}.md")))
}

/// The stack file's text, or `None` when there is none.
pub fn read(dir: &Path, id: &str) -> Result<Option<String>> {
    match std::fs::read_to_string(file(dir, id)?) {
        Ok(t) => Ok(Some(t)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e.into()),
    }
}

/// Atomic write (temp file + rename), dir `0700`, file `0600` — the briefs' writer.
pub fn write(dir: &Path, id: &str, text: &str) -> Result<()> {
    file(dir, id)?;
    brief::write(dir, id, text)
}

pub fn remove(dir: &Path, id: &str) -> Result<bool> {
    match std::fs::remove_file(file(dir, id)?) {
        Ok(()) => Ok(true),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(e.into()),
    }
}

/// Every stack id with a file in `dir`, sorted. A missing dir is no stacks.
pub fn list_ids(dir: &Path) -> Vec<String> {
    let mut ids: Vec<String> = std::fs::read_dir(dir)
        .map(|rd| {
            rd.flatten()
                .filter_map(|e| {
                    let name = e.file_name().to_string_lossy().to_string();
                    let id = name.strip_suffix(".md")?;
                    is_stack_id(id).then(|| id.to_string())
                })
                .collect()
        })
        .unwrap_or_default();
    ids.sort();
    ids
}

/// Every readable stack in `dir`, by id. The file name is the id (a frontmatter `stack` that
/// disagrees is overridden).
pub fn load_all(dir: &Path) -> Vec<Stack> {
    list_ids(dir)
        .into_iter()
        .filter_map(|id| load(dir, &id).ok().flatten())
        .collect()
}

pub fn load(dir: &Path, id: &str) -> Result<Option<Stack>> {
    Ok(read(dir, id)?.map(|t| {
        let mut s = parse_stack(&t);
        s.meta.insert("stack".into(), Value::String(id.into()));
        s
    }))
}

/// Stamp `updated` and write the canonical text.
pub fn save(dir: &Path, stack: &mut Stack, now: &str, paths: &Paths) -> Result<()> {
    stack
        .meta
        .insert("updated".into(), Value::String(now.into()));
    write(dir, &stack.id(), &serialize_stack(stack, paths))
}

// ------------------------------------------------------------------ the shape

/// `stack` on a `fleet list --json` row.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StackRef {
    pub id: String,
    pub label: String,
    /// The stack's home host: where its file is (frontmatter `host`).
    #[serde(default)]
    pub host: String,
}

/// One entry of the frontmatter `members` array.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Member {
    pub session: String,
    #[serde(default)]
    pub host: Option<String>,
    /// The display title at the last sync (informational).
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub added: Option<String>,
    /// `null` while the session is live; when it was seen gone, after that.
    #[serde(default)]
    pub closed: Option<String>,
    /// The session's cwd (`~/…`), so its transcript path survives the session.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    /// The first prompt (≤ 160 chars), so the Sessions line survives the session.
    #[serde(
        default,
        rename = "firstPrompt",
        skip_serializing_if = "Option::is_none"
    )]
    pub first_prompt: Option<String>,
    /// Keys added later (or by another writer) are kept.
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

impl Member {
    pub fn is_live(&self) -> bool {
        self.closed.is_none()
    }
    fn short(&self) -> String {
        self.session.chars().take(8).collect()
    }
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Stack {
    /// Frontmatter, in file order, without `members` (unknown keys included).
    pub meta: Map<String, Value>,
    pub members: Vec<Member>,
    /// Text before the first `## ` heading, the generated header blockquote left out.
    pub preamble: String,
    pub summary: String,
    pub resources_text: String,
    /// `Some` when there is a `## Notes` section (even an empty one).
    pub notes: Option<String>,
    /// Any other `## ` section, in file order.
    pub extra: Vec<Extra>,
}

impl Stack {
    pub fn meta_str(&self, key: &str) -> Option<String> {
        match self.meta.get(key)? {
            Value::Null => None,
            Value::String(s) if s.is_empty() => None,
            Value::String(s) => Some(s.clone()),
            v => Some(v.to_string()),
        }
    }
    pub fn id(&self) -> String {
        self.meta_str("stack").unwrap_or_default()
    }
    /// The label, else the id.
    pub fn label(&self) -> String {
        self.meta_str("label").unwrap_or_else(|| self.id())
    }
    /// The home host (frontmatter `host`); `local` when a file predates the key.
    pub fn home(&self, local: &str) -> String {
        self.meta_str("host").unwrap_or_else(|| local.to_string())
    }
    pub fn stack_ref(&self, local: &str) -> StackRef {
        StackRef {
            id: self.id(),
            label: self.label(),
            host: self.home(local),
        }
    }
    pub fn member(&self, session_id: &str) -> Option<&Member> {
        self.members.iter().find(|m| m.session == session_id)
    }
    /// The host a member runs on: its `host`, else the stack's home.
    pub fn member_host(&self, m: &Member, local: &str) -> String {
        m.host
            .as_deref()
            .map(str::trim)
            .filter(|h| !h.is_empty())
            .map(str::to_string)
            .unwrap_or_else(|| self.home(local))
    }
    pub fn counts(&self) -> (usize, usize) {
        let live = self.members.iter().filter(|m| m.is_live()).count();
        (live, self.members.len() - live)
    }
    /// The Resources bullets, parsed like a brief's (a `Folder:` line gets the kind `Folder`).
    pub fn resources(&self) -> Vec<brief::Resource> {
        self.resources_text
            .split('\n')
            .filter_map(brief::bullet)
            .filter(|b| !b.trim().is_empty())
            .map(|b| {
                let mut r = brief::parse_resource_line(b);
                if r.kind.is_none() && starts_ci(r.text.trim(), "folder:") {
                    r.kind = Some("Folder".into());
                }
                r
            })
            .collect()
    }
}

fn starts_ci(s: &str, prefix: &str) -> bool {
    s.get(..prefix.len())
        .is_some_and(|h| h.eq_ignore_ascii_case(prefix))
}

// ------------------------------------------------------------------ parse / serialize

fn members_of(v: Option<Value>) -> Vec<Member> {
    match v {
        Some(Value::Array(a)) => a
            .into_iter()
            .filter_map(|m| serde_json::from_value::<Member>(m).ok())
            .filter(|m| !m.session.trim().is_empty())
            .collect(),
        _ => Vec::new(),
    }
}

/// Drop the generated header (a leading blockquote carrying [`HEADER_MARK`]) from the
/// text before the first heading; anything else there is a human's and kept.
fn strip_header<'a>(pre: &[&'a str]) -> Vec<&'a str> {
    let quoted = |l: &str| l.trim_start().starts_with('>');
    let Some(at) = pre
        .iter()
        .position(|l| quoted(l) && l.contains(HEADER_MARK))
    else {
        return pre.to_vec();
    };
    let mut start = at;
    while start > 0 && quoted(pre[start - 1]) {
        start -= 1;
    }
    let mut end = at;
    while end < pre.len() && quoted(pre[end]) {
        end += 1;
    }
    let mut out = pre[..start].to_vec();
    out.extend_from_slice(&pre[end..]);
    out
}

/// Parse a StackBrief (with or without frontmatter). Tolerant like briefs: headings
/// case-insensitive, sections in any order or missing, other sections kept.
pub fn parse_stack(text: &str) -> Stack {
    let (mut meta, body) = brief::parse_frontmatter(text);
    let members = members_of(meta.remove("members"));
    #[derive(Clone, Copy)]
    enum Cur {
        Pre,
        Summary,
        Resources,
        Sessions,
        Notes,
        Extra(usize),
    }
    let mut pre: Vec<&str> = Vec::new();
    let mut summary: Vec<&str> = Vec::new();
    let mut resources: Vec<&str> = Vec::new();
    let mut notes: Option<Vec<&str>> = None;
    let mut extra: Vec<(String, Vec<&str>)> = Vec::new();
    let mut cur = Cur::Pre;
    for line in body.split('\n') {
        if let Some(h) = brief::heading(line) {
            cur = match h.to_lowercase().as_str() {
                "summary" => Cur::Summary,
                "resources" => Cur::Resources,
                "sessions" => Cur::Sessions,
                "notes" => {
                    notes.get_or_insert_with(Vec::new);
                    Cur::Notes
                }
                _ => {
                    extra.push((h, Vec::new()));
                    Cur::Extra(extra.len() - 1)
                }
            };
            continue;
        }
        match cur {
            Cur::Pre => pre.push(line),
            Cur::Summary => summary.push(line),
            Cur::Resources => resources.push(line),
            // Regenerated from `members` on every write.
            Cur::Sessions => {}
            Cur::Notes => notes.get_or_insert_with(Vec::new).push(line),
            Cur::Extra(i) => extra[i].1.push(line),
        }
    }
    Stack {
        meta,
        members,
        preamble: brief::join(&strip_header(&pre)),
        summary: brief::join(&summary),
        resources_text: brief::join(&resources),
        notes: notes.map(|n| brief::join(&n)),
        extra: extra
            .into_iter()
            .map(|(heading, lines)| Extra {
                heading,
                body: brief::join(&lines),
            })
            .collect(),
    }
}

/// Where the paths printed in `## Sessions` point.
#[derive(Debug, Clone)]
pub struct Paths {
    pub briefs_dir: PathBuf,
    /// `~/.claude`
    pub claude_home: PathBuf,
    /// For `~/…` display and expansion.
    pub home: Option<PathBuf>,
}

impl Paths {
    pub fn current() -> Self {
        let home = dirs::home_dir();
        Paths {
            briefs_dir: brief::briefs_dir().unwrap_or_else(|_| {
                home.clone()
                    .unwrap_or_default()
                    .join(".local/state/fleet/briefs")
            }),
            claude_home: discovery::claude_home(),
            home,
        }
    }
    /// `/home/x/a` → `~/a` (else as is).
    pub fn tilde(&self, p: &str) -> String {
        if let Some(h) = self.home.as_ref().and_then(|h| h.to_str())
            && !h.is_empty()
            && let Some(rest) = p.strip_prefix(h)
            && (rest.is_empty() || rest.starts_with('/'))
        {
            return format!("~{rest}");
        }
        p.to_string()
    }
    /// `~/a` → `/home/x/a` (else as is).
    pub fn expand(&self, p: &str) -> String {
        match (p.strip_prefix('~'), &self.home) {
            (Some(rest), Some(h)) if rest.is_empty() || rest.starts_with('/') => {
                format!("{}{rest}", h.display())
            }
            _ => p.to_string(),
        }
    }
    pub fn brief_path(&self, session_id: &str) -> PathBuf {
        self.briefs_dir.join(format!("{session_id}.md"))
    }
    /// `~/.claude/projects/<cwd with / and . as ->/<id>.jsonl` for an absolute cwd.
    pub fn transcript_path(&self, cwd: &str, session_id: &str) -> Option<PathBuf> {
        let abs = self.expand(cwd);
        abs.starts_with('/').then(|| {
            self.claude_home
                .join("projects")
                .join(discovery::encode_cwd(&abs))
                .join(format!("{session_id}.jsonl"))
        })
    }
}

/// `2026-09-29T10:00:00.000Z` → `2026-09-29 10:00` (UTC); anything else as is.
fn short_time(iso: &str) -> String {
    chrono::DateTime::parse_from_rfc3339(iso)
        .map(|t| {
            t.with_timezone(&chrono::Utc)
                .format("%Y-%m-%d %H:%M")
                .to_string()
        })
        .unwrap_or_else(|_| iso.to_string())
}

/// The blockquote every machine write puts at the top of the body.
pub fn header(label: &str, id: &str) -> String {
    format!(
        "> {HEADER_MARK} **{label}** (`{id}`). Every session in\n\
> this stack reads this file when it starts. Keep **Summary** and **Resources** current for your\n\
> siblings (PRs, worktrees, folders, decisions). **Sessions** is maintained by `fleet stack` — do\n\
> not edit it; read a sibling's brief or transcript from there when you need to know what it did.\n"
    )
}

/// `## Sessions`, regenerated from the members. A member on another host than the stack's
/// home says so (`on <host>`): the brief and transcript paths are on that host.
pub fn sessions_section(stack: &Stack, paths: &Paths) -> String {
    if stack.members.is_empty() {
        return "(no sessions yet)".into();
    }
    let stack_cwd = stack.meta_str("cwd");
    let home = stack.home("");
    let mut out = Vec::new();
    for m in &stack.members {
        let name = m
            .name
            .as_deref()
            .map(brief::one_line)
            .filter(|n| !n.is_empty())
            .unwrap_or_else(|| m.short());
        let mut state = match &m.closed {
            None => "live".to_string(),
            Some(c) => format!("closed {}", short_time(c)),
        };
        let host = stack.member_host(m, &home);
        if host != home {
            state.push_str(&format!(", on {host}"));
        }
        let mut parts = Vec::new();
        if let Some(a) = &m.added {
            parts.push(format!("added {}", short_time(a)));
        }
        let brief_path = paths.brief_path(&m.session);
        parts.push(format!(
            "brief `{}`",
            paths.tilde(&brief_path.display().to_string())
        ));
        if let Some(t) = m
            .cwd
            .as_deref()
            .or(stack_cwd.as_deref())
            .and_then(|c| paths.transcript_path(c, &m.session))
        {
            parts.push(format!(
                "transcript `{}`",
                paths.tilde(&t.display().to_string())
            ));
        }
        out.push(format!(
            "- **{name}** (`{}`, {state}) — {}",
            m.short(),
            parts.join(" · ")
        ));
        if let Some(p) = m
            .first_prompt
            .as_deref()
            .map(brief::one_line)
            .filter(|p| !p.is_empty())
        {
            out.push(format!(
                "  first prompt: \"{}\"",
                clip(&p, FIRST_PROMPT_CAP)
            ));
        }
    }
    out.join("\n")
}

fn sec(name: &str, text: &str) -> String {
    if text.is_empty() {
        format!("## {name}\n")
    } else {
        format!("## {name}\n{text}\n")
    }
}

/// The body: header, Summary, Resources, Sessions (regenerated), Notes, the rest.
pub fn serialize_body(stack: &Stack, paths: &Paths) -> String {
    let mut out = header(&stack.label(), &stack.id());
    out.push('\n');
    if !stack.preamble.is_empty() {
        out.push_str(&format!("{}\n\n", stack.preamble));
    }
    out.push_str(&sec("Summary", &stack.summary));
    out.push('\n');
    out.push_str(&sec("Resources", &stack.resources_text));
    out.push('\n');
    out.push_str(&sec("Sessions", &sessions_section(stack, paths)));
    if let Some(n) = &stack.notes {
        out.push('\n');
        out.push_str(&sec("Notes", n));
    }
    for s in &stack.extra {
        out.push('\n');
        out.push_str(&sec(&s.heading, &s.body));
    }
    out
}

pub fn serialize_frontmatter(stack: &Stack) -> String {
    let mut meta = stack.meta.clone();
    meta.insert(
        "members".into(),
        serde_json::to_value(&stack.members).unwrap_or(Value::Array(Vec::new())),
    );
    brief::serialize_frontmatter_ordered(&meta, &META_ORDER)
}

/// The whole file, canonical.
pub fn serialize_stack(stack: &Stack, paths: &Paths) -> String {
    serialize_frontmatter(stack) + &serialize_body(stack, paths)
}

// ------------------------------------------------------------------ building and editing

/// `s` on one line, at most `cap` characters (an ellipsis when cut).
pub fn clip(s: &str, cap: usize) -> String {
    let s = brief::one_line(s);
    if s.chars().count() <= cap {
        return s;
    }
    let cut: String = s.chars().take(cap.saturating_sub(1)).collect();
    format!("{}…", cut.trim_end())
}

/// A `--label` / model label: one line, trimmed of markdown noise, non-empty, capped.
pub fn clean_label(raw: &str) -> Option<String> {
    let l = brief::one_line(raw);
    let l = l.trim_matches(|c: char| c.is_whitespace() || "*_`\"'#".contains(c));
    (!l.is_empty()).then(|| clip(l, LABEL_CAP))
}

/// A member for a live session.
pub fn member_for(s: &Session, host: &str, now: &str, paths: &Paths) -> Option<Member> {
    let session = s.session_id.clone().filter(|id| brief::is_session_id(id))?;
    Some(Member {
        session,
        host: Some(host.to_string()),
        name: Some(s.headline()),
        added: Some(now.to_string()),
        closed: None,
        cwd: s.cwd.as_deref().map(|c| paths.tilde(c)),
        first_prompt: s
            .title
            .as_deref()
            .map(|t| clip(t, FIRST_PROMPT_CAP))
            .filter(|t| !t.is_empty()),
        extra: Map::new(),
    })
}

/// What a new stack is made of.
pub struct NewStack<'a> {
    pub id: &'a str,
    pub label: &'a str,
    pub host: &'a str,
    /// `~/…`
    pub cwd: &'a str,
    pub now: &'a str,
    pub member: Member,
    pub content: &'a Generated,
    /// The model wrote Summary / Resources (stamps `generatedAt`).
    pub generated: bool,
}

pub fn new_stack(n: NewStack) -> Stack {
    let mut meta = Map::new();
    for (k, v) in [
        ("stack", n.id),
        ("label", n.label),
        ("host", n.host),
        ("cwd", n.cwd),
        ("created", n.now),
        ("updated", n.now),
    ] {
        meta.insert(k.into(), Value::String(v.into()));
    }
    if n.generated {
        meta.insert("generatedAt".into(), Value::String(n.now.into()));
    }
    Stack {
        meta,
        members: vec![n.member],
        preamble: String::new(),
        summary: n.content.summary.clone(),
        resources_text: n
            .content
            .resources
            .iter()
            .map(|r| format!("- {r}"))
            .collect::<Vec<_>>()
            .join("\n"),
        notes: Some(String::new()),
        extra: Vec::new(),
    }
}

/// A human edit (`fleet stack set`, the web PUT): the submitted body is authoritative for
/// everything but the header and `## Sessions` (regenerated); the stored frontmatter's machine
/// keys win; `label` is taken from submitted frontmatter when it has one; keys the edit adds
/// are kept; `editedAt` is stamped. The caller then [`save`]s (which stamps `updated`).
pub fn human_edit(before: &Stack, markdown: &str, now: &str) -> Result<Stack> {
    if markdown.len() > MAX_EDIT_BYTES {
        return Err(Error::Other(format!(
            "StackBrief too long (max {} kB)",
            MAX_EDIT_BYTES / 1024
        )));
    }
    let incoming = parse_stack(markdown);
    let mut meta = before.meta.clone();
    for (k, v) in &incoming.meta {
        if !MACHINE_KEYS.contains(&k.as_str()) && !meta.contains_key(k) {
            meta.insert(k.clone(), v.clone());
        }
    }
    if let Some(l) = incoming
        .meta
        .get("label")
        .and_then(Value::as_str)
        .and_then(clean_label)
    {
        meta.insert("label".into(), Value::String(l));
    }
    meta.insert("editedAt".into(), Value::String(now.into()));
    Ok(Stack {
        meta,
        members: before.members.clone(),
        ..incoming
    })
}

/// Add (or re-open) a live session as a member. → whether anything changed.
pub fn add_member(stack: &mut Stack, m: Member) -> bool {
    if let Some(old) = stack.members.iter_mut().find(|x| x.session == m.session) {
        let mut next = old.clone();
        next.closed = None;
        next.name = m.name.or(next.name);
        next.cwd = next.cwd.or(m.cwd);
        next.first_prompt = next.first_prompt.or(m.first_prompt);
        let changed = *old != next;
        *old = next;
        return changed;
    }
    stack.members.push(m);
    true
}

// ------------------------------------------------------------------ membership sync

/// What a sync (or a view) sees: the live sessions of the hosts that answered.
#[derive(Debug, Clone)]
pub struct Observed<'a> {
    /// Live rows; a row's host is its `host`, else `local`.
    pub rows: &'a [Session],
    /// Hosts whose sessions `rows` lists in full. A member of any other host is not judged:
    /// it stays as it was.
    pub hosts_ok: &'a HashSet<String>,
    /// Session ids dormant on `local` (left by a reboot, `snapshot::dormant_session_ids`).
    pub dormant: &'a HashSet<String>,
    /// This machine's host name (what rows and members without a `host` are under).
    pub local: &'a str,
}

impl<'a> Observed<'a> {
    /// `(host, session id)` → its row.
    pub fn live(&self) -> HashMap<(String, &'a str), &'a Session> {
        self.rows
            .iter()
            .filter_map(|r| {
                let id = r.session_id.as_deref()?;
                let host = r
                    .host
                    .as_deref()
                    .filter(|h| !h.is_empty())
                    .unwrap_or(self.local)
                    .to_string();
                Some(((host, id), r))
            })
            .collect()
    }
}

/// Parse a sessions document for `--input`: a `list --json` array (rows under their `host`,
/// else `local`; every host present counts as answered) or a `/api/fleet`-shaped
/// `{ hosts: [{ name, ok, sessions }] }` (hosts with `ok: false` are not observed). Rows that
/// don't parse are skipped.
pub fn parse_input(
    v: &Value,
    local: &str,
) -> std::result::Result<(Vec<Session>, HashSet<String>), String> {
    let mut rows = Vec::new();
    let mut hosts_ok = HashSet::new();
    let row = |r: &Value, host: &str| -> Option<Session> {
        let mut s: Session = serde_json::from_value(r.clone()).ok()?;
        if s.host.as_deref().is_none_or(|h| h.trim().is_empty()) {
            s.host = Some(host.to_string());
        }
        Some(s)
    };
    if let Some(list) = v.as_array() {
        for r in list {
            let host = r
                .get("host")
                .and_then(Value::as_str)
                .filter(|h| !h.trim().is_empty())
                .unwrap_or(local)
                .to_string();
            if let Some(s) = row(r, &host) {
                hosts_ok.insert(host);
                rows.push(s);
            }
        }
        return Ok((rows, hosts_ok));
    }
    let hosts = v
        .get("hosts")
        .and_then(Value::as_array)
        .ok_or("expected a session array or an object with `hosts`")?;
    for h in hosts {
        let Some(name) = h.get("name").and_then(Value::as_str) else {
            continue;
        };
        if h.get("ok").and_then(Value::as_bool) == Some(false) {
            continue;
        }
        hosts_ok.insert(name.to_string());
        for r in h
            .get("sessions")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            if let Some(s) = row(r, name) {
                rows.push(s);
            }
        }
    }
    Ok((rows, hosts_ok))
}

/// Reconcile one stack's members with the live sessions: gone → `closed = now` (once), live →
/// `name` refreshed from the display title (and re-opened if it had been marked closed — a
/// resumed session). A `dormant` one (left by a reboot, waiting for `fleet restore`) is
/// neither: it stays as it was. So does a member of a host not in `hosts_ok` — its state is
/// unknown here. → whether anything changed.
pub fn sync_stack(stack: &mut Stack, obs: &Observed, now: &str, paths: &Paths) -> bool {
    let live = obs.live();
    let home = stack.home(obs.local);
    let mut changed = false;
    let members = std::mem::take(&mut stack.members);
    let mut next = Vec::with_capacity(members.len());
    for mut m in members {
        let before = m.clone();
        let host = stack.member_host(&m, &home);
        if obs.hosts_ok.contains(&host) {
            match live.get(&(host.clone(), m.session.as_str())) {
                Some(s) => {
                    m.closed = None;
                    m.name = Some(s.headline());
                    if m.cwd.is_none() {
                        m.cwd = s.cwd.as_deref().map(|c| paths.tilde(c));
                    }
                    if m.first_prompt.is_none() {
                        m.first_prompt = s
                            .title
                            .as_deref()
                            .map(|t| clip(t, FIRST_PROMPT_CAP))
                            .filter(|t| !t.is_empty());
                    }
                }
                None if host == obs.local && obs.dormant.contains(&m.session) => {}
                None => {
                    if m.closed.is_none() {
                        m.closed = Some(now.to_string());
                    }
                }
            }
        }
        changed |= m != before;
        next.push(m);
    }
    stack.members = next;
    changed
}

/// [`sync_stack`] for every stack in `dir`; rewrites only the files that changed (unless
/// `write` is false: dry-run). → the ids that changed.
pub fn sync(
    dir: &Path,
    obs: &Observed,
    now: &str,
    paths: &Paths,
    write: bool,
) -> Result<Vec<String>> {
    let mut changed = Vec::new();
    for mut s in load_all(dir) {
        if sync_stack(&mut s, obs, now, paths) {
            if write {
                save(dir, &mut s, now, paths)?;
            }
            changed.push(s.id());
        }
    }
    Ok(changed)
}

/// The stack a session belongs to (a live membership wins over a closed one; then the most
/// recently added).
pub fn stack_of<'a>(stacks: &'a [Stack], session_id: &str) -> Option<&'a Stack> {
    let mut best: Option<(&Stack, bool, String)> = None;
    for s in stacks {
        if let Some(m) = s.member(session_id) {
            let key = (m.is_live(), m.added.clone().unwrap_or_default());
            if best
                .as_ref()
                .is_none_or(|(_, live, added)| key > (*live, added.clone()))
            {
                best = Some((s, key.0, key.1));
            }
        }
    }
    best.map(|(s, _, _)| s)
}

/// Put `stack: { id, label, host }` on every row that is a member of a stack in `dir` (`host`
/// = the stack's home, `local` for a file without one). Read-only: no sync, no writes (what
/// `fleet list` does). Only this host's files are known here: a membership in a stack homed
/// elsewhere is stamped by the web server, from the home host's list.
pub fn stamp_stacks_in(dir: &Path, local: &str, rows: &mut [Session]) {
    let stacks = load_all(dir);
    if stacks.is_empty() {
        return;
    }
    for r in rows.iter_mut() {
        if let Some(id) = r.session_id.as_deref() {
            r.stack = stack_of(&stacks, id).map(|s| s.stack_ref(local));
        }
    }
}

/// [`stamp_stacks_in`] the configured stacks dir, under this machine's name (config `self`).
pub fn stamp_stacks(rows: &mut [Session]) {
    if let Ok(dir) = stacks_dir() {
        let local = crate::core::config::get().self_name();
        stamp_stacks_in(&dir, &local, rows);
    }
}

// ------------------------------------------------------------------ resolving `<stack>`

fn ambiguous(q: &str, hits: &[&Stack]) -> Error {
    Error::tagged_exit(
        EXIT_AMBIGUOUS,
        "stack_ambiguous",
        format!(
            "\"{q}\" matches {} stacks: {} — be more specific",
            hits.len(),
            hits.iter()
                .map(|s| format!("{} ({})", s.label(), s.id()))
                .collect::<Vec<_>>()
                .join(", ")
        ),
    )
}

/// `<stack>`: exact id (with or without `st-`) → label (exact, then case-insensitive prefix,
/// then substring) → a member's session id (whole, or a prefix of ≥ 8) → a session target
/// (`discovery::resolve_in` over `rows`) that is a member. Two hits on one rung is an error
/// (exit 2) listing the candidates; no hit is exit 3.
pub fn resolve<'a>(stacks: &'a [Stack], rows: &[Session], q: &str) -> Result<&'a Stack> {
    let raw = q.trim();
    let ql = raw.to_lowercase();
    if ql.is_empty() {
        return Err(Error::Other("no stack given".into()));
    }
    type Rung<'r> = Box<dyn Fn(&Stack) -> bool + 'r>;
    let rungs: Vec<Rung> = vec![
        Box::new(|s: &Stack| s.id() == ql || s.id() == format!("st-{ql}")),
        Box::new(|s: &Stack| s.meta_str("label").as_deref() == Some(raw)),
        Box::new(|s: &Stack| {
            s.meta_str("label")
                .is_some_and(|l| l.to_lowercase().starts_with(&ql))
        }),
        Box::new(|s: &Stack| {
            s.meta_str("label")
                .is_some_and(|l| l.to_lowercase().contains(&ql))
        }),
        Box::new(|s: &Stack| {
            s.members.iter().any(|m| {
                let id = m.session.to_lowercase();
                id == ql || (ql.len() >= 8 && id.starts_with(&ql))
            })
        }),
    ];
    // `pid:<n>` / `id:<session_id>` name a session exactly: no label rung may claim them,
    // and `id:` matches a member's whole session id (a gone member's too), never a prefix.
    let exact = discovery::exact_target(raw);
    if let Some(Ok(discovery::Exact::Id(id))) = &exact {
        let hits: Vec<&Stack> = stacks
            .iter()
            .filter(|s| s.members.iter().any(|m| m.session.eq_ignore_ascii_case(id)))
            .collect();
        match hits.len() {
            0 => {}
            1 => return Ok(hits[0]),
            _ => return Err(ambiguous(raw, &hits)),
        }
    }
    for rung in rungs.iter().filter(|_| exact.is_none()) {
        let hits: Vec<&Stack> = stacks.iter().filter(|s| rung(s)).collect();
        match hits.len() {
            0 => continue,
            1 => return Ok(hits[0]),
            _ => return Err(ambiguous(raw, &hits)),
        }
    }
    match discovery::resolve_target_in(rows.to_vec(), raw) {
        Ok(s) => {
            if let Some(id) = s.session_id.as_deref() {
                let hits: Vec<&Stack> =
                    stacks.iter().filter(|st| st.member(id).is_some()).collect();
                match hits.len() {
                    0 => {}
                    1 => return Ok(hits[0]),
                    _ => return Err(ambiguous(raw, &hits)),
                }
            }
            Err(Error::tagged_exit(
                EXIT_NOTHING,
                "stack_not_found",
                format!("{} is not in a stack", s.headline()),
            ))
        }
        Err(e @ discovery::TargetError::Ambiguous(_)) => {
            Err(Error::tagged_exit(EXIT_AMBIGUOUS, e.kind(), e.to_string()))
        }
        // `pid:` / `id:` can only mean a session: say which way it failed.
        Err(e) if exact.is_some() => Err(Error::tagged_exit(EXIT_NOTHING, e.kind(), e.to_string())),
        Err(_) => Err(Error::tagged_exit(
            EXIT_NOTHING,
            "stack_not_found",
            format!("no stack matches \"{raw}\""),
        )),
    }
}

// ------------------------------------------------------------------ the context line

/// The sentence every session spawned into a stack on its home host starts with.
pub fn context_line(path: &Path) -> String {
    format!(
        "You're running in the session stack with shared context: {}.",
        path.display()
    )
}

/// The same sentence for a session on another host than the stack's `home`: the file is not
/// there, `fleet -H <home> stack …` reads and writes it.
pub fn remote_context_line(id: &str, home: &str) -> String {
    format!(
        "You're running in the session stack with shared context: stack {id} on host {home}. \
Read it with `fleet -H {home} stack show {id}` and update it with `fleet -H {home} stack set \
{id}` (the whole markdown on stdin; `fleet -H {home} stack show {id} --json` has it under \
`markdown`)."
    )
}

/// `line`, a space, then `prompt` (nothing after the line when the prompt is empty).
pub fn prompt_after(line: &str, prompt: &str) -> String {
    let p = prompt.trim();
    if p.is_empty() {
        line.to_string()
    } else {
        format!("{line} {p}")
    }
}

/// [`context_line`], a space, then `prompt` (nothing after the period when it is empty).
pub fn stack_prompt(path: &Path, prompt: &str) -> String {
    prompt_after(&context_line(path), prompt)
}

// ------------------------------------------------------------------ the JSON view

/// What `view` needs beyond the stack.
pub struct ViewCtx<'a> {
    /// The name rows are tagged with (the caller's name for this host).
    pub host_label: &'a str,
    /// The live sessions seen (this host's, or every host's through `--input`).
    pub obs: &'a Observed<'a>,
    pub paths: &'a Paths,
    /// The stack file.
    pub path: &'a Path,
}

/// StackView: the `--json` shape of every `fleet stack` command (and the web API's). A member
/// on a host not observed here keeps `live` from its `closed` (unknown otherwise: `status`
/// null, `briefPath` null — the brief is on that host).
pub fn view(stack: &Stack, c: &ViewCtx) -> Value {
    let live = c.obs.live();
    let home = stack.home(c.obs.local);
    let get = |k: &str| stack.meta.get(k).cloned().unwrap_or(Value::Null);
    let cwd = stack.meta_str("cwd");
    let abs = cwd
        .as_deref()
        .map(|p| c.paths.expand(p))
        .filter(|p| p.starts_with('/'));
    let members: Vec<Value> = stack
        .members
        .iter()
        .map(|m| {
            let mut v = serde_json::to_value(m).unwrap_or(Value::Null);
            let host = stack.member_host(m, &home);
            let observed = c.obs.hosts_ok.contains(&host);
            let row = live.get(&(host.clone(), m.session.as_str()));
            let brief_path = (host == c.obs.local).then(|| c.paths.brief_path(&m.session));
            if let Value::Object(o) = &mut v {
                o.insert("host".into(), json!(host));
                o.insert(
                    "live".into(),
                    json!(if observed { row.is_some() } else { m.is_live() }),
                );
                o.insert(
                    "dormant".into(),
                    json!(
                        row.is_none() && host == c.obs.local && c.obs.dormant.contains(&m.session)
                    ),
                );
                o.insert("status".into(), json!(row.map(|r| r.status.clone())));
                o.insert(
                    "briefPath".into(),
                    json!(brief_path.as_ref().map(|p| p.display().to_string())),
                );
                o.insert(
                    "briefExists".into(),
                    json!(brief_path.as_ref().is_some_and(|p| p.is_file())),
                );
            }
            v
        })
        .collect();
    json!({
        "host": c.host_label,
        "id": stack.id(),
        "label": stack.label(),
        "path": c.path.display().to_string(),
        "cwd": cwd,
        "absCwd": abs,
        "created": get("created"),
        "updated": get("updated"),
        "generatedAt": get("generatedAt"),
        "editedAt": get("editedAt"),
        "contextLine": context_line(c.path),
        "remoteContextLine": remote_context_line(&stack.id(), &home),
        "members": members,
        "markdown": serialize_stack(stack, c.paths),
        "body": serialize_body(stack, c.paths),
        "parsed": {
            "summary": stack.summary,
            "resources": stack.resources().iter().map(|r| json!({
                "kind": r.kind, "label": r.label, "url": r.url, "path": r.path, "text": r.text,
                "branch": r.branch, "linked": r.linked,
            })).collect::<Vec<_>>(),
            "notes": stack.notes.clone().unwrap_or_default(),
        },
    })
}

// ------------------------------------------------------------------ generation

/// What the creation call sees about the source session.
#[derive(Debug, Clone, Default)]
pub struct GenInput {
    pub title: String,
    /// `~/…`
    pub cwd: String,
    pub host: String,
    /// The Git resource line (without `- `), when the cwd is in a checkout.
    pub git: Option<String>,
    pub first_prompt: Option<String>,
    /// The session brief's body, when there is one.
    pub brief: Option<String>,
    /// The tail of the conversation ([`conversation_tail`]).
    pub conversation: String,
}

/// Summary + Resources (+ a label) for a new StackBrief.
#[derive(Debug, Clone, PartialEq)]
pub struct Generated {
    pub label: Option<String>,
    pub summary: String,
    /// Resource bullets without the leading `- `.
    pub resources: Vec<String>,
}

pub fn generation_prompt(i: &GenInput) -> String {
    let mut out = String::from(
        "You write the shared context file for a \"session stack\": several Claude Code sessions \
working on one stream of work, side by side. The stack starts from the session described below; \
sibling sessions will join it later, and every one of them reads this file first.\n\n\
Answer with exactly this and nothing else — no preamble, no code fence:\n\n\
Label: <2-5 words naming the stream of work>\n\n\
## Summary\n\
<2-4 sentences, at most 1000 characters: what the stack as a whole is about and where it happens \
(repo, branch or worktree, host). Generalise to the stream of work — do NOT list this one \
session's steps or todos.>\n\n\
## Resources\n\
- <Kind>: <value>\n\n\
Resources: one bullet per thing really present in the input below — a PR, an issue, the git \
branch/worktree, a folder, a spec, a link. Kinds: PR, Issue, Git, Folder, Spec, File, Artifact, \
Link. A value is a markdown link [label](url) or a code span `path`. Never invent one; leave \
things out rather than guess.\n\n--- Source session ---\n",
    );
    out.push_str(&format!("Title: {}\n", i.title));
    out.push_str(&format!("Host: {}\n", i.host));
    out.push_str(&format!("Directory: {}\n", i.cwd));
    if let Some(g) = &i.git {
        out.push_str(&format!("{g}\n"));
    }
    if let Some(p) = i.first_prompt.as_deref().filter(|p| !p.trim().is_empty()) {
        out.push_str(&format!("First prompt: {}\n", clip(p, 600)));
    }
    if let Some(b) = i.brief.as_deref().filter(|b| !b.trim().is_empty()) {
        out.push_str("\n--- Its session brief (may be stale) ---\n");
        let b: String = b.chars().take(3000).collect();
        out.push_str(b.trim());
        out.push('\n');
    }
    if !i.conversation.trim().is_empty() {
        out.push_str("\n--- Recent conversation (newest last) ---\n");
        out.push_str(i.conversation.trim());
        out.push('\n');
    }
    out
}

/// The model's answer → Summary / Resources / label. `None` for anything without a usable
/// `## Summary` (the caller falls back to [`skeleton`]).
pub fn parse_answer(text: &str) -> Option<Generated> {
    let lines: Vec<&str> = text
        .lines()
        .filter(|l| !l.trim_start().starts_with("```"))
        .collect();
    let mut label = None;
    let mut summary: Vec<&str> = Vec::new();
    let mut resources: Vec<&str> = Vec::new();
    let mut saw_summary = false;
    #[derive(Clone, Copy, PartialEq)]
    enum Cur {
        None,
        Summary,
        Resources,
        Other,
    }
    let mut cur = Cur::None;
    for line in &lines {
        let t = line.trim().trim_start_matches(['*', '_']).trim_start();
        if label.is_none() && cur != Cur::Summary && starts_ci(t, "label") {
            let rest = t[5..].trim_start_matches(['*', '_']).trim_start();
            if let Some(v) = rest.strip_prefix(':') {
                label = clean_label(v).filter(|l| {
                    let words = l.split_whitespace().count();
                    (1..=8).contains(&words)
                });
                continue;
            }
        }
        if let Some(h) = brief::heading(line) {
            cur = match h.to_lowercase().as_str() {
                "summary" => {
                    saw_summary = true;
                    Cur::Summary
                }
                "resources" => Cur::Resources,
                _ => Cur::Other,
            };
            continue;
        }
        match cur {
            Cur::Summary => summary.push(line),
            Cur::Resources => resources.push(line),
            _ => {}
        }
    }
    let summary = brief::join(&summary);
    if !saw_summary || summary.trim().is_empty() || summary.contains("<2-4 sentences") {
        return None;
    }
    let summary = if summary.chars().count() > SUMMARY_CAP {
        let cut: String = summary.chars().take(SUMMARY_CAP - 1).collect();
        format!("{}…", cut.trim_end())
    } else {
        summary
    };
    let resources = resources
        .iter()
        .filter_map(|l| brief::bullet(l))
        .map(str::trim)
        .filter(|b| !b.is_empty() && !b.starts_with('<') && !b.contains("<Kind>"))
        .take(30)
        .map(String::from)
        .collect();
    Some(Generated {
        label,
        summary,
        resources,
    })
}

/// The StackBrief without a model: "Started from <title> in <cwd>." + the Git line + Folder.
pub fn skeleton(title: &str, cwd: &str, git: Option<&str>) -> Generated {
    complete(
        Generated {
            label: None,
            summary: format!("Started from {} in {cwd}.", brief::one_line(title)),
            resources: Vec::new(),
        },
        cwd,
        git,
    )
}

/// Make sure the Git line (first) and a Folder line are among the resources.
pub fn complete(mut g: Generated, cwd: &str, git: Option<&str>) -> Generated {
    if let Some(git) = git
        && !g.resources.iter().any(|r| starts_ci(r, "git:"))
    {
        g.resources.insert(0, git.to_string());
    }
    if !cwd.is_empty() && !g.resources.iter().any(|r| starts_ci(r, "folder:")) {
        g.resources.push(format!("Folder: `{cwd}`"));
    }
    g
}

/// One `claude -p --model <model>` call → the parsed answer (completed with the Git / Folder
/// lines). `Err` says why nothing usable came back.
pub fn generate(input: &GenInput, model: &str) -> std::result::Result<Generated, String> {
    let prompt = generation_prompt(input);
    let cancel = AtomicBool::new(false);
    let answer = crate::core::naming::ask_claude(&prompt, model, GEN_TIMEOUT, &cancel)
        .map_err(|e| e.to_string())?;
    let g = parse_answer(&answer)
        .ok_or_else(|| "the model's answer had no usable ## Summary".to_string())?;
    Ok(complete(g, &input.cwd, input.git.as_deref()))
}

/// The Git resource line for a directory (`` Git: `<branch>` · worktree `<root>` `` / `` · repo
/// `<root>` ``, `detached` for no branch), or `None` outside git.
pub fn git_line(dir: &str, paths: &Paths) -> Option<String> {
    let run = |args: &[&str]| -> Option<String> {
        let mut c = std::process::Command::new("git");
        c.arg("-C").arg(dir).args(args);
        let got = crate::core::hosts::capture(c, Duration::from_secs(3)).ok()?;
        (got.code == Some(0)).then_some(got.stdout)
    };
    let out = run(&[
        "rev-parse",
        "--path-format=absolute",
        "--show-toplevel",
        "--git-dir",
        "--git-common-dir",
    ])?;
    let parts: Vec<&str> = out.lines().map(str::trim).collect();
    let [top, git_dir, common] = parts[..] else {
        return None;
    };
    if !top.starts_with('/') {
        return None;
    }
    let linked = git_dir.trim_end_matches('/') != common.trim_end_matches('/');
    let branch = run(&["symbolic-ref", "--short", "-q", "HEAD"])
        .map(|b| b.trim().to_string())
        .filter(|b| !b.is_empty());
    let b = match branch {
        Some(b) => format!("`{b}`"),
        None => "detached".into(),
    };
    Some(format!(
        "Git: {b} · {} `{}`",
        if linked { "worktree" } else { "repo" },
        paths.tilde(top)
    ))
}

/// Text of a transcript entry's message: a string, or its `text` blocks (tool blocks skipped).
fn entry_text(v: &Value) -> String {
    let content = &v["message"]["content"];
    if let Some(s) = content.as_str() {
        return s.to_string();
    }
    content
        .as_array()
        .map(|a| {
            a.iter()
                .filter(|b| b["type"] == "text")
                .filter_map(|b| b["text"].as_str())
                .collect::<Vec<_>>()
                .join("\n")
        })
        .unwrap_or_default()
}

/// The conversation from a transcript's tail, newest kept: user prompts and turn-ending
/// assistant text (main thread only, tool calls and results skipped), each clipped, at most
/// `max_chars` in all.
pub fn conversation_tail(path: &Path, max_chars: usize) -> String {
    use std::io::{Read, Seek, SeekFrom};
    const TAIL: u64 = 1024 * 1024;
    let Ok(mut f) = std::fs::File::open(path) else {
        return String::new();
    };
    let size = f.metadata().map(|m| m.len()).unwrap_or(0);
    let start = size.saturating_sub(TAIL);
    if f.seek(SeekFrom::Start(start)).is_err() {
        return String::new();
    }
    let mut buf = Vec::new();
    if f.read_to_end(&mut buf).is_err() {
        return String::new();
    }
    let text = String::from_utf8_lossy(&buf);
    let text = if start > 0 {
        text.split_once('\n').map_or("", |(_, r)| r).to_string()
    } else {
        text.to_string()
    };
    conversation_from(&text, max_chars)
}

/// [`conversation_tail`] over JSONL text.
pub fn conversation_from(jsonl: &str, max_chars: usize) -> String {
    let mut turns: Vec<(&'static str, String)> = Vec::new();
    let mut pending: Option<String> = None;
    for line in jsonl.lines() {
        let Ok(v) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        if v["isSidechain"] == true {
            continue;
        }
        match v["type"].as_str() {
            Some("user") => {
                let t = entry_text(&v);
                let t = t.trim();
                if t.is_empty() || t.starts_with('<') {
                    continue;
                }
                if let Some(a) = pending.take() {
                    turns.push(("Assistant", a));
                }
                turns.push(("User", t.to_string()));
            }
            Some("assistant") => {
                let t = entry_text(&v);
                if !t.trim().is_empty() {
                    pending = Some(t.trim().to_string());
                }
            }
            _ => {}
        }
    }
    if let Some(a) = pending {
        turns.push(("Assistant", a));
    }
    let mut out: Vec<String> = Vec::new();
    let mut used = 0;
    for (who, t) in turns.iter().rev() {
        let item = format!("{who}: {}", clip(t, 1200));
        let n = item.chars().count() + 2;
        if used + n > max_chars && !out.is_empty() {
            break;
        }
        used += n;
        out.push(item);
        if used >= max_chars {
            break;
        }
    }
    out.reverse();
    out.join("\n\n")
}

/// Session ids of every member of every stack (for "is it in another stack?").
pub fn members_index(stacks: &[Stack]) -> HashSet<String> {
    stacks
        .iter()
        .flat_map(|s| s.members.iter().map(|m| m.session.clone()))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const SID: &str = "aaaaaaaa-1111-2222-3333-444444444444";
    const SID2: &str = "bbbbbbbb-1111-2222-3333-444444444444";
    const NOW: &str = "2026-09-29T10:00:00.000Z";
    const LATER: &str = "2026-09-29T11:30:00.000Z";

    fn paths() -> Paths {
        Paths {
            briefs_dir: PathBuf::from("/home/u/.local/state/fleet/briefs"),
            claude_home: PathBuf::from("/home/u/.claude"),
            home: Some(PathBuf::from("/home/u")),
        }
    }

    fn row(sid: &str, name: &str) -> Session {
        Session {
            pid: 7,
            session_id: Some(sid.into()),
            name: Some(name.into()),
            name_source: Some("user".into()),
            cwd: Some("/home/u/Code/project".into()),
            status: "idle".into(),
            title: Some("Fix the login redirect loop".into()),
            ..Default::default()
        }
    }

    fn sample() -> Stack {
        let p = paths();
        let m = member_for(&row(SID, "login-redirect"), "laptop", NOW, &p).unwrap();
        let g = skeleton(
            "login-redirect",
            "~/Code/project",
            Some("Git: `fix-login` · repo `~/Code/project`"),
        );
        new_stack(NewStack {
            id: "st-1a2b3c4d",
            label: "Login redirect fix",
            host: "laptop",
            cwd: "~/Code/project",
            now: NOW,
            member: m,
            content: &g,
            generated: false,
        })
    }

    #[test]
    fn serialize_parse_round_trip() {
        let p = paths();
        let s = sample();
        let text = serialize_stack(&s, &p);
        assert!(text.starts_with("---\nstack: st-1a2b3c4d\nlabel: Login redirect fix\nhost: laptop\ncwd: ~/Code/project\ncreated: 2026-09-29T10:00:00.000Z\nupdated: 2026-09-29T10:00:00.000Z\nmembers: [{\"session\":\"aaaaaaaa-"), "{text}");
        assert!(
            text.contains("\"closed\":null"),
            "closed is written as null: {text}"
        );
        assert!(text.contains(
            "> Shared context for the session stack **Login redirect fix** (`st-1a2b3c4d`)."
        ));
        assert!(text.contains("## Summary\nStarted from login-redirect in ~/Code/project.\n"));
        assert!(text.contains(
            "## Resources\n- Git: `fix-login` · repo `~/Code/project`\n- Folder: `~/Code/project`\n"
        ));
        assert!(text.contains(
            "- **login-redirect** (`aaaaaaaa`, live) — added 2026-09-29 10:00 · brief `~/.local/state/fleet/briefs/aaaaaaaa-1111-2222-3333-444444444444.md` · transcript `~/.claude/projects/-home-u-Code-project/aaaaaaaa-1111-2222-3333-444444444444.jsonl`\n  first prompt: \"Fix the login redirect loop\""
        ), "{text}");
        assert!(text.ends_with("## Notes\n"), "{text}");
        let back = parse_stack(&text);
        assert_eq!(back, s);
        assert_eq!(
            serialize_stack(&back, &p),
            text,
            "canonical is a fixed point"
        );
        let r = back.resources();
        assert_eq!(r[0].kind.as_deref(), Some("Git"));
        assert_eq!(r[1].kind.as_deref(), Some("Folder"));
        assert_eq!(r[1].path.as_deref(), Some("~/Code/project"));
    }

    #[test]
    fn sessions_are_regenerated_and_human_sections_kept() {
        let p = paths();
        let mut s = sample();
        let text = serialize_stack(&s, &p);
        // A human (or a session) edits the file directly: Sessions scribbled on, notes added,
        // an extra section, a preamble line, unknown frontmatter.
        let edited = text
            .replace("## Sessions\n", "## Sessions\n- garbage someone wrote\n")
            .replace(
                "## Notes\n",
                "## Notes\nremember the flag\n\n## Decisions\n- no retries\n",
            )
            .replace("---\n> Shared", "---\nA human line.\n\n> Shared")
            .replace("members:", "zeta: kept\nmembers:");
        let mut back = parse_stack(&edited);
        assert_eq!(back.notes.as_deref(), Some("remember the flag"));
        assert_eq!(back.extra[0].heading, "Decisions");
        assert_eq!(back.meta["zeta"], json!("kept"));
        back.members = s.members.clone();
        let out = serialize_stack(&back, &p);
        assert!(!out.contains("garbage"), "{out}");
        assert!(out.contains("## Notes\nremember the flag\n\n## Decisions\n- no retries\n"));
        assert!(out.contains("zeta: kept"));
        // The header stays one, whatever sat above it.
        assert_eq!(out.matches(HEADER_MARK).count(), 1, "{out}");
        s.members.clear();
        assert!(serialize_stack(&s, &p).contains("## Sessions\n(no sessions yet)\n"));
    }

    #[test]
    fn tolerant_parsing() {
        let s = parse_stack("## resources\n* Link: [x](https://x.dev)\n## SUMMARY\nhello\n");
        assert_eq!(s.summary, "hello");
        assert_eq!(s.resources()[0].url.as_deref(), Some("https://x.dev"));
        assert!(s.members.is_empty());
        assert_eq!(s.notes, None);
        // A broken members value is no members, not an error.
        let s = parse_stack("---\nstack: st-00000000\nmembers: [{\"x\":1}, 5]\n---\n");
        assert!(s.members.is_empty());
    }

    #[test]
    fn human_edit_keeps_machine_keys_and_takes_the_label() {
        let p = paths();
        let before = sample();
        let text = serialize_stack(&before, &p)
            .replace("label: Login redirect fix", "label: Auth rework")
            .replace("created: 2026-09-29T10:00:00.000Z", "created: 1999")
            .replace("members: [", "extra: new\nmembers: [{\"session\":\"x\"},")
            .replace(
                "Started from login-redirect in ~/Code/project.",
                "The auth stream.",
            )
            .replace("## Notes\n", "## Notes\nA note.\n");
        let after = human_edit(&before, &text, LATER).unwrap();
        assert_eq!(after.label(), "Auth rework");
        assert_eq!(after.meta_str("created").as_deref(), Some(NOW));
        assert_eq!(after.meta_str("editedAt").as_deref(), Some(LATER));
        assert_eq!(after.meta["extra"], json!("new"));
        assert_eq!(
            after.members, before.members,
            "members stay the stored ones"
        );
        assert_eq!(after.summary, "The auth stream.");
        assert_eq!(after.notes.as_deref(), Some("A note."));
        assert_eq!(after.resources_text, before.resources_text);
        // A body without frontmatter keeps the label.
        let plain = human_edit(&before, "## Summary\nx\n", LATER).unwrap();
        assert_eq!(plain.label(), "Login redirect fix");
        assert_eq!(plain.resources_text, "", "the body is authoritative");
        assert!(human_edit(&before, &"x".repeat(MAX_EDIT_BYTES + 1), LATER).is_err());
    }

    /// A laptop-only observation: `rows` are laptop's, nothing dormant.
    struct Seen {
        rows: Vec<Session>,
        ok: HashSet<String>,
        dormant: HashSet<String>,
    }

    impl Seen {
        fn laptop(rows: Vec<Session>) -> Self {
            Seen {
                rows,
                ok: ["laptop".to_string()].into(),
                dormant: HashSet::new(),
            }
        }
        fn obs(&self) -> Observed<'_> {
            Observed {
                rows: &self.rows,
                hosts_ok: &self.ok,
                dormant: &self.dormant,
                local: "laptop",
            }
        }
    }

    #[test]
    fn sync_marks_closed_refreshes_names_and_reopens() {
        let p = paths();
        let mut s = sample();
        s.members
            .push(member_for(&row(SID2, "login-tests"), "laptop", NOW, &p).unwrap());
        let seen = Seen::laptop(vec![row(SID, "renamed-one")]);
        assert!(sync_stack(&mut s, &seen.obs(), LATER, &p));
        assert_eq!(s.members[0].name.as_deref(), Some("renamed-one"));
        assert_eq!(s.members[0].closed, None);
        assert_eq!(s.members[1].closed.as_deref(), Some(LATER));
        // Nothing changed → no write.
        assert!(!sync_stack(
            &mut s,
            &seen.obs(),
            "2026-09-29T12:00:00.000Z",
            &p
        ));
        assert_eq!(s.members[1].closed.as_deref(), Some(LATER), "closed once");
        let text = serialize_stack(&s, &p);
        assert!(
            text.contains("(`bbbbbbbb`, closed 2026-09-29 11:30)"),
            "{text}"
        );
        // Back (resumed): re-opened.
        let seen = Seen::laptop(vec![row(SID, "renamed-one"), row(SID2, "login-tests")]);
        assert!(sync_stack(&mut s, &seen.obs(), LATER, &p));
        assert_eq!(s.members[1].closed, None);
    }

    #[test]
    fn sync_leaves_dormant_members_open() {
        let p = paths();
        let mut s = sample();
        s.members
            .push(member_for(&row(SID2, "login-tests"), "laptop", NOW, &p).unwrap());
        // After a reboot nothing is live; SID2 is dormant, SID was closed on purpose.
        let mut seen = Seen::laptop(Vec::new());
        seen.dormant = [SID2.to_string()].into();
        assert!(sync_stack(&mut s, &seen.obs(), LATER, &p));
        assert_eq!(s.members[0].closed.as_deref(), Some(LATER));
        assert_eq!(s.members[1].closed, None, "dormant is not closed");
        let d = tempfile::tempdir().unwrap();
        let path = d.path().join("st.md");
        let v = view(
            &s,
            &ViewCtx {
                host_label: "laptop",
                obs: &seen.obs(),
                paths: &p,
                path: &path,
            },
        );
        assert_eq!(v["members"][0]["dormant"], false);
        assert_eq!(v["members"][1]["dormant"], true);
        assert_eq!(v["members"][1]["live"], false);
        // Resumed: live again, no longer dormant.
        seen.rows = vec![row(SID2, "login-tests")];
        let v = view(
            &s,
            &ViewCtx {
                host_label: "laptop",
                obs: &seen.obs(),
                paths: &p,
                path: &path,
            },
        );
        assert_eq!(v["members"][1]["dormant"], false);
        assert_eq!(v["members"][1]["live"], true);
    }

    #[test]
    fn members_on_other_hosts_are_judged_only_against_their_host() {
        let p = paths();
        let mut s = sample();
        // SID2 runs on the workstation (a sibling spawned there).
        let mut r2 = row(SID2, "login-tests");
        r2.host = Some("workstation".into());
        s.members
            .push(member_for(&r2, "workstation", NOW, &p).unwrap());
        let text = serialize_stack(&s, &p);
        assert!(
            text.contains("(`bbbbbbbb`, live, on workstation)"),
            "{text}"
        );
        // Laptop alone: the workstation member is left as it was.
        let seen = Seen::laptop(vec![row(SID, "login-redirect")]);
        assert!(!sync_stack(&mut s, &seen.obs(), LATER, &p));
        assert_eq!(s.members[1].closed, None);
        let d = tempfile::tempdir().unwrap();
        let path = d.path().join("st.md");
        let v = view(
            &s,
            &ViewCtx {
                host_label: "laptop",
                obs: &seen.obs(),
                paths: &p,
                path: &path,
            },
        );
        assert_eq!(v["members"][1]["host"], "workstation");
        assert_eq!(v["members"][1]["live"], true, "unknown here: from `closed`");
        assert_eq!(v["members"][1]["status"], Value::Null);
        assert_eq!(v["members"][1]["briefPath"], Value::Null);
        assert_eq!(v["members"][1]["briefExists"], false);
        assert_eq!(v["members"][0]["host"], "laptop");
        assert_eq!(
            v["remoteContextLine"],
            remote_context_line("st-1a2b3c4d", "laptop")
        );
        // Both hosts observed (the web server's merged fleet): gone on the workstation → closed.
        let mut seen = Seen::laptop(vec![row(SID, "login-redirect")]);
        seen.ok.insert("workstation".into());
        assert!(sync_stack(&mut s, &seen.obs(), LATER, &p));
        assert_eq!(s.members[1].closed.as_deref(), Some(LATER));
        // The same id live on the laptop is not the workstation's session.
        seen.rows.push(row(SID2, "login-tests"));
        assert!(!sync_stack(&mut s, &seen.obs(), LATER, &p));
        assert_eq!(s.members[1].closed.as_deref(), Some(LATER));
        // Back on the workstation: re-opened.
        seen.rows.push(r2.clone());
        assert!(sync_stack(&mut s, &seen.obs(), LATER, &p));
        assert_eq!(s.members[1].closed, None);
    }

    #[test]
    fn input_parses_a_list_and_a_fleet_body() {
        let (rows, ok) = parse_input(
            &json!([{ "session_id": SID, "name": "a" }, { "session_id": SID2, "host": "workstation" }]),
            "laptop",
        )
        .unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].host.as_deref(), Some("laptop"));
        assert_eq!(rows[1].host.as_deref(), Some("workstation"));
        assert_eq!(ok, ["laptop".to_string(), "workstation".to_string()].into());
        let (rows, ok) = parse_input(
            &json!({ "hosts": [
                { "name": "laptop", "ok": true, "sessions": [{ "session_id": SID, "stack": { "id": "st-1a2b3c4d", "label": "x", "host": "laptop" } }] },
                { "name": "workstation", "ok": false, "error": "unreachable", "sessions": [] },
                { "name": "office", "sessions": [{ "pid": 5 }] }
            ] }),
            "laptop",
        )
        .unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[1].host.as_deref(), Some("office"));
        assert_eq!(ok, ["laptop".to_string(), "office".to_string()].into());
        assert!(parse_input(&json!({ "nope": 1 }), "laptop").is_err());
    }

    #[test]
    fn sync_writes_only_changed_files() {
        let d = tempfile::tempdir().unwrap();
        let p = paths();
        let mut s = sample();
        save(d.path(), &mut s, NOW, &p).unwrap();
        let seen = Seen::laptop(vec![row(SID, "login-redirect")]);
        assert!(
            sync(d.path(), &seen.obs(), LATER, &p, true)
                .unwrap()
                .is_empty()
        );
        assert_eq!(
            load(d.path(), "st-1a2b3c4d")
                .unwrap()
                .unwrap()
                .meta_str("updated")
                .as_deref(),
            Some(NOW)
        );
        let seen = Seen::laptop(Vec::new());
        assert_eq!(
            sync(d.path(), &seen.obs(), LATER, &p, true).unwrap(),
            ["st-1a2b3c4d"]
        );
        let back = load(d.path(), "st-1a2b3c4d").unwrap().unwrap();
        assert_eq!(back.members[0].closed.as_deref(), Some(LATER));
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(d.path().join("st-1a2b3c4d.md"))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600);
        // stamp_stacks is read-only.
        let mut rows = vec![row(SID, "x"), row("cccccccc-0000", "y")];
        stamp_stacks_in(d.path(), "laptop", &mut rows);
        assert_eq!(
            rows[0].stack,
            Some(StackRef {
                id: "st-1a2b3c4d".into(),
                label: "Login redirect fix".into(),
                host: "laptop".into(),
            })
        );
        assert_eq!(rows[1].stack, None);
        let v = serde_json::to_value(&rows[1]).unwrap();
        assert!(v.get("stack").unwrap().is_null(), "null, not skipped");
    }

    #[test]
    fn resolve_rungs_and_ambiguity() {
        let mk = |id: &str, label: &str, sid: &str| {
            let mut s = sample();
            s.meta.insert("stack".into(), json!(id));
            s.meta.insert("label".into(), json!(label));
            s.members[0].session = sid.into();
            s
        };
        let stacks = vec![
            mk("st-00000001", "Login fix", SID),
            mk("st-00000002", "Login tests", SID2),
            mk("st-00000003", "Billing", "cccccccc-1111"),
        ];
        let id = |r: Result<&Stack>| r.map(|s| s.id()).map_err(|e| (e.code(), e.to_string()));
        assert_eq!(
            id(resolve(&stacks, &[], "st-00000002")).unwrap(),
            "st-00000002"
        );
        assert_eq!(
            id(resolve(&stacks, &[], "00000003")).unwrap(),
            "st-00000003"
        );
        assert_eq!(
            id(resolve(&stacks, &[], "Login fix")).unwrap(),
            "st-00000001"
        );
        assert_eq!(id(resolve(&stacks, &[], "bill")).unwrap(), "st-00000003");
        assert_eq!(id(resolve(&stacks, &[], "tests")).unwrap(), "st-00000002");
        let (code, msg) = id(resolve(&stacks, &[], "login")).unwrap_err();
        assert_eq!(code, EXIT_AMBIGUOUS);
        assert!(
            msg.contains("matches 2 stacks") && msg.contains("Login tests (st-00000002)"),
            "{msg}"
        );
        // A member's session id (prefix ≥ 8).
        assert_eq!(
            id(resolve(&stacks, &[], "bbbbbbbb")).unwrap(),
            "st-00000002"
        );
        // A session target that is a member.
        let rows = vec![
            row(SID, "redirect-work"),
            row("dddddddd-0000", "other-work"),
        ];
        assert_eq!(
            id(resolve(&stacks, &rows, "redirect")).unwrap(),
            "st-00000001"
        );
        let (code, msg) = id(resolve(&stacks, &rows, "other-work")).unwrap_err();
        assert_eq!(code, EXIT_NOTHING);
        assert!(msg.contains("not in a stack"), "{msg}");
        let (code, _) = id(resolve(&stacks, &rows, "work")).unwrap_err();
        assert_eq!(code, EXIT_AMBIGUOUS, "two sessions on one rung");
        let (code, _) = id(resolve(&stacks, &rows, "nothing-like-it")).unwrap_err();
        assert_eq!(code, EXIT_NOTHING);
    }

    // `id:` / `pid:` name a session exactly: no label rung claims them, and `id:` takes a
    // member's whole id — a gone member's too — but never a prefix of it.
    #[test]
    fn resolve_exact_session_forms() {
        let mk = |id: &str, label: &str, sid: &str| {
            let mut s = sample();
            s.meta.insert("stack".into(), json!(id));
            s.meta.insert("label".into(), json!(label));
            s.members[0].session = sid.into();
            s
        };
        let stacks = vec![
            mk("st-00000001", "Login", SID),
            mk("st-00000002", "pid:8 notes", SID2),
        ];
        let id = |r: Result<&Stack>| r.map(|s| s.id()).map_err(|e| e.code());
        // Gone member (no live rows), whole id, any case.
        assert_eq!(
            id(resolve(
                &stacks,
                &[],
                &format!("id:{}", SID2.to_uppercase())
            ))
            .unwrap(),
            "st-00000002"
        );
        assert_eq!(
            id(resolve(&stacks, &[], "id:bbbbbbbb")),
            Err(EXIT_NOTHING),
            "a prefix is not an exact id"
        );
        // `pid:7` is the live session with pid 7 (a member of st-1); `pid:8` is no live pid, and
        // the label "pid:8 notes" must not claim it as a prefix.
        let rows = vec![row(SID, "redirect-work")];
        assert_eq!(id(resolve(&stacks, &rows, "pid:7")).unwrap(), "st-00000001");
        assert_eq!(id(resolve(&stacks, &rows, "pid:8")), Err(EXIT_NOTHING));
    }

    #[test]
    fn context_line_is_exact() {
        let p = Path::new("/state/fleet/stacks/st-1a2b3c4d.md");
        assert_eq!(
            context_line(p),
            "You're running in the session stack with shared context: /state/fleet/stacks/st-1a2b3c4d.md."
        );
        assert_eq!(
            stack_prompt(p, "  Write the tests. "),
            "You're running in the session stack with shared context: /state/fleet/stacks/st-1a2b3c4d.md. Write the tests."
        );
        assert_eq!(stack_prompt(p, "  "), context_line(p));
    }

    #[test]
    fn model_answers_parse_and_garbage_falls_back() {
        let a = "Label: **Login redirect fix**\n\n## Summary\nFixing the login redirect loop in ~/Code/project.\n\n## Resources\n- PR: [o/r#12](https://github.com/o/r/pull/12)\n- <Kind>: <value>\nnot a bullet\n";
        let g = parse_answer(a).unwrap();
        assert_eq!(g.label.as_deref(), Some("Login redirect fix"));
        assert_eq!(
            g.summary,
            "Fixing the login redirect loop in ~/Code/project."
        );
        assert_eq!(
            g.resources,
            ["PR: [o/r#12](https://github.com/o/r/pull/12)"]
        );
        let g = complete(
            g,
            "~/Code/project",
            Some("Git: `x` · repo `~/Code/project`"),
        );
        assert_eq!(g.resources[0], "Git: `x` · repo `~/Code/project`");
        assert_eq!(g.resources.last().unwrap(), "Folder: `~/Code/project`");
        // Fenced, no label, long summary → capped.
        let long = format!("```\n## Summary\n{}\n```", "word ".repeat(400));
        let g = parse_answer(&long).unwrap();
        assert_eq!(g.label, None);
        assert!(g.summary.chars().count() <= SUMMARY_CAP);
        // Garbage → None → the skeleton.
        for bad in [
            "",
            "I cannot help with that.",
            "## Resources\n- PR: x\n",
            "## Summary\n\n",
        ] {
            assert_eq!(parse_answer(bad), None, "{bad}");
        }
        let s = skeleton("login-redirect", "~/Code/project", None);
        assert_eq!(s.summary, "Started from login-redirect in ~/Code/project.");
        assert_eq!(s.resources, ["Folder: `~/Code/project`"]);
        // The prompt carries the inputs.
        let p = generation_prompt(&GenInput {
            title: "login-redirect".into(),
            cwd: "~/Code/project".into(),
            host: "laptop".into(),
            git: Some("Git: `x` · repo `~/Code/project`".into()),
            first_prompt: Some("fix it".into()),
            brief: Some("## Summary\nbrief text".into()),
            conversation: "User: hi".into(),
        });
        for want in [
            "Title: login-redirect",
            "Host: laptop",
            "Git: `x`",
            "First prompt: fix it",
            "brief text",
            "User: hi",
            "Label:",
        ] {
            assert!(p.contains(want), "{want}");
        }
    }

    #[test]
    fn conversation_keeps_prompts_and_turn_ending_text() {
        let lines = [
            json!({"type":"user","message":{"role":"user","content":"first ask"}}),
            json!({"type":"assistant","message":{"content":[{"type":"text","text":"thinking out loud"},{"type":"tool_use","name":"Bash"}]}}),
            json!({"type":"user","message":{"role":"user","content":[{"type":"tool_result","content":"out"}]}}),
            json!({"type":"assistant","message":{"content":[{"type":"text","text":"done with it"}]}}),
            json!({"type":"assistant","isSidechain":true,"message":{"content":[{"type":"text","text":"subagent"}]}}),
            json!({"type":"user","message":{"role":"user","content":"<command-name>/clear</command-name>"}}),
            json!({"type":"user","message":{"role":"user","content":"second ask"}}),
        ];
        let text = lines
            .iter()
            .map(|l| l.to_string())
            .collect::<Vec<_>>()
            .join("\n");
        let c = conversation_from(&text, 6000);
        assert_eq!(
            c,
            "User: first ask\n\nAssistant: done with it\n\nUser: second ask"
        );
        // Newest kept under the cap.
        assert_eq!(conversation_from(&text, 20), "User: second ask");
    }

    #[test]
    fn ids_and_labels() {
        let d = tempfile::tempdir().unwrap();
        let id = new_id(d.path());
        assert!(is_stack_id(&id), "{id}");
        for bad in [
            "st-1234567",
            "st-1234567g",
            "st-ABCDEF12",
            "1a2b3c4d",
            "st-1a2b3c4d.md",
        ] {
            assert!(!is_stack_id(bad), "{bad}");
        }
        assert_eq!(
            clean_label("  **Auth rework**\n"),
            Some("Auth rework".into())
        );
        assert_eq!(clean_label(" ** "), None);
        assert!(file(d.path(), "../x").is_err());
    }
}
