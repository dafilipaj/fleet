//! Session briefs: the file format and the files (docs/architecture.md → "Session briefs").
//!
//! A port of the web server's `web/lib/brief-format.mjs` (the reference implementation) plus the
//! human-edit rules of its `PUT` (`web/lib/briefs.mjs`). The server owns generation — nothing in
//! here calls a model. Both sides pin the same fixtures (`testdata/briefs/`), so a change to the
//! format has to land in both.

use std::io::Write as _;
use std::path::{Path, PathBuf};

use serde_json::{Map, Value, json};

use crate::error::{Error, Result};

/// Frontmatter keys in the order they are written; unknown keys follow, as found.
const META_ORDER: [&str; 10] = [
    "session",
    "host",
    "cwd",
    "updated",
    "generatedThrough",
    "generatedAt",
    "editedAt",
    "todos",
    "dismissed",
    "git",
];
pub const MAX_DISMISSED: usize = 200;
/// The server refuses a longer edit (413); so do we.
pub const MAX_EDIT_CHARS: usize = 60000;
const RESOURCE_KINDS: [&str; 10] = [
    "Origin", "PR", "Issue", "Artifact", "Spec", "File", "Git", "Branch", "Worktree", "Link",
];

// ------------------------------------------------------------------ storage

/// Where brief files live: `$FLEET_BRIEFS_DIR`, else `${XDG_STATE_HOME:-~/.local/state}/fleet/briefs`.
pub fn briefs_dir() -> Result<PathBuf> {
    let env = |k: &str| std::env::var(k).ok().filter(|v| !v.is_empty());
    let expand = |p: &str| PathBuf::from(crate::core::tools::expand_tilde(p));
    let dir = if let Some(d) = env("FLEET_BRIEFS_DIR") {
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
        base.join("fleet").join("briefs")
    };
    if !dir.is_absolute() {
        return Err(Error::Other(format!(
            "briefs dir must be an absolute path: {}",
            dir.display()
        )));
    }
    Ok(dir)
}

/// `/^[A-Za-z0-9][A-Za-z0-9_-]{0,127}$/` — what may name a brief file.
pub fn is_session_id(id: &str) -> bool {
    let b = id.as_bytes();
    !b.is_empty()
        && b.len() <= 128
        && b[0].is_ascii_alphanumeric()
        && b.iter()
            .all(|c| c.is_ascii_alphanumeric() || *c == b'_' || *c == b'-')
}

pub fn file(dir: &Path, id: &str) -> Result<PathBuf> {
    if !is_session_id(id) {
        return Err(Error::Other(format!("not a session id: {id}")));
    }
    Ok(dir.join(format!("{id}.md")))
}

/// The brief's text, or `None` when there is none yet.
pub fn read(dir: &Path, id: &str) -> Result<Option<String>> {
    match std::fs::read_to_string(file(dir, id)?) {
        Ok(t) => Ok(Some(t)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e.into()),
    }
}

/// Write atomically: a temp file next to the target, then rename. Dir `0700`, file `0600`.
pub fn write(dir: &Path, id: &str, text: &str) -> Result<()> {
    write_private(&file(dir, id)?, text)
}

/// [`write`] for any path: the parent dir is created `0700`, the file lands `0600` via a
/// temp file next to it and a rename, so a reader never sees half of it. Shared by every
/// fleet state file that is not the config (briefs, the session snapshot).
pub fn write_private(target: &Path, text: &str) -> Result<()> {
    use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
    let dir = target
        .parent()
        .ok_or_else(|| Error::Other(format!("no parent dir: {}", target.display())))?;
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(dir)?;
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or(0);
    let base = target
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_default();
    let tmp = dir.join(format!("{base}.{}.{nonce:08x}.tmp", std::process::id()));
    let res = (|| -> std::io::Result<()> {
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&tmp)?;
        f.write_all(text.as_bytes())?;
        f.sync_all()?;
        std::fs::rename(&tmp, target)
    })();
    if let Err(e) = res {
        let _ = std::fs::remove_file(&tmp);
        return Err(e.into());
    }
    Ok(())
}

/// The stored brief (parsed), or the empty skeleton. → (brief, exists).
pub fn load(dir: &Path, id: &str) -> Result<(Brief, bool)> {
    Ok(match read(dir, id)? {
        Some(t) => (parse_brief(&t), true),
        None => (empty_brief(id), false),
    })
}

/// `new Date().toISOString()`: millisecond precision, `Z`.
pub fn iso_now() -> String {
    chrono::Utc::now()
        .format("%Y-%m-%dT%H:%M:%S%.3fZ")
        .to_string()
}

/// What every write does last (the server's `save`): stamp `session` and `updated`.
pub fn save(dir: &Path, id: &str, brief: &mut Brief, now: &str) -> Result<()> {
    brief
        .meta
        .insert("session".into(), Value::String(id.into()));
    brief
        .meta
        .insert("updated".into(), Value::String(now.into()));
    write(dir, id, &serialize_brief(brief))
}

// ------------------------------------------------------------------ the parsed shape

#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct Resource {
    pub kind: Option<String>,
    pub label: Option<String>,
    pub url: Option<String>,
    pub path: Option<String>,
    pub text: String,
    /// `Git` lines: the branch (`None` when detached); `None` for every other kind.
    pub branch: Option<String>,
    /// `Git` lines: a linked worktree (`true`) or the main checkout (`false`).
    pub linked: Option<bool>,
    pub key: String,
}

/// A `## Todos` checkbox line.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct Todo {
    pub done: bool,
    pub text: String,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct Extra {
    pub heading: String,
    pub body: String,
}

#[derive(Debug, Clone, Default, PartialEq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Brief {
    /// Frontmatter, in file order (unknown keys included).
    pub meta: Map<String, Value>,
    pub preamble: String,
    pub summary: String,
    pub resources_text: String,
    pub todos_text: String,
    pub extra: Vec<Extra>,
    pub resources: Vec<Resource>,
    pub todos: Vec<Todo>,
}

impl Brief {
    /// A frontmatter value as display text (`${v}` in JS for the scalar kinds).
    pub fn meta_str(&self, key: &str) -> Option<String> {
        match self.meta.get(key)? {
            Value::Null => None,
            Value::String(s) => Some(s.clone()),
            v => Some(v.to_string()),
        }
    }
}

// ------------------------------------------------------------------ small regex ports

fn is_ws(c: char) -> bool {
    c.is_whitespace()
}

/// `String(v).replace(/[\r\n]+/g, ' ').trim()`
pub(crate) fn one_line(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut in_break = false;
    for c in s.chars() {
        if c == '\r' || c == '\n' {
            if !in_break {
                out.push(' ');
            }
            in_break = true;
        } else {
            out.push(c);
            in_break = false;
        }
    }
    out.trim().to_string()
}

fn is_int(s: &str) -> bool {
    let d = s.strip_prefix('-').unwrap_or(s);
    !d.is_empty() && d.bytes().all(|b| b.is_ascii_digit())
}

fn starts_json(s: &str) -> bool {
    s.starts_with('[') || s.starts_with('{') || s.starts_with('"')
}

/// `v.replace(/\s+#.*$/, '')` — cut at the first whitespace run followed by `#`.
fn strip_comment(v: &str) -> &str {
    let idx: Vec<(usize, char)> = v.char_indices().collect();
    let mut i = 0;
    while i < idx.len() {
        if is_ws(idx[i].1) {
            let mut j = i;
            while j < idx.len() && is_ws(idx[j].1) {
                j += 1;
            }
            if j < idx.len() && idx[j].1 == '#' {
                return &v[..idx[i].0];
            }
            i = j;
        } else {
            i += 1;
        }
    }
    v
}

/// `/^([A-Za-z][\w-]*)\s*:\s*(.*)$/` → (key, raw value).
fn meta_line(line: &str) -> Option<(&str, &str)> {
    let first = line.chars().next()?;
    if !first.is_ascii_alphabetic() {
        return None;
    }
    let end = line
        .find(|c: char| !(c.is_ascii_alphanumeric() || c == '_' || c == '-'))
        .unwrap_or(line.len());
    let key = &line[..end];
    let rest = line[end..].trim_start_matches(is_ws);
    let rest = rest.strip_prefix(':')?;
    Some((key, rest))
}

fn number(s: &str) -> Value {
    match s.parse::<i64>() {
        Ok(n) => json!(n),
        Err(_) => s
            .parse::<f64>()
            .ok()
            .and_then(serde_json::Number::from_f64)
            .map(Value::Number)
            .unwrap_or_else(|| Value::String(s.into())),
    }
}

// ------------------------------------------------------------------ frontmatter

/// Split `---\n…\n---` off the top. → (meta, body). No frontmatter → empty meta.
pub fn parse_frontmatter(text: &str) -> (Map<String, Value>, String) {
    let src = text.replace("\r\n", "\n");
    let mut meta = Map::new();
    let Some(inner_start) = src.strip_prefix("---\n").map(|_| 4) else {
        return (meta, src);
    };
    // The first `\n---[ \t]*` that ends a line (or the text) closes the block.
    let mut close = None;
    let mut from = inner_start;
    while let Some(off) = src[from..].find("\n---") {
        let p = from + off;
        let after = src[p + 4..].trim_start_matches([' ', '\t']);
        if after.is_empty() || after.starts_with('\n') {
            let end = src.len() - after.len() + usize::from(after.starts_with('\n'));
            close = Some((p, end));
            break;
        }
        from = p + 1;
    }
    let Some((p, end)) = close else {
        return (meta, src);
    };
    for line in src[inner_start..p].split('\n') {
        let Some((key, raw)) = meta_line(line) else {
            continue;
        };
        let mut v = raw.trim();
        if !starts_json(v) {
            v = strip_comment(v);
        }
        if v.is_empty() || v == "null" || v == "~" {
            continue;
        }
        let value = if is_int(v) {
            number(v)
        } else if starts_json(v) {
            serde_json::from_str(v).unwrap_or_else(|_| Value::String(v.into()))
        } else {
            Value::String(v.into())
        };
        meta.insert(key.to_string(), value);
    }
    (meta, src[end..].to_string())
}

pub fn serialize_frontmatter(meta: &Map<String, Value>) -> String {
    serialize_frontmatter_ordered(meta, &META_ORDER)
}

/// [`serialize_frontmatter`] with another key order (the stack files use their own).
pub fn serialize_frontmatter_ordered(meta: &Map<String, Value>, order: &[&str]) -> String {
    let mut keys: Vec<&str> = order
        .iter()
        .copied()
        .filter(|k| meta.contains_key(*k))
        .collect();
    keys.extend(
        meta.keys()
            .map(String::as_str)
            .filter(|k| !order.contains(k)),
    );
    let mut lines = Vec::new();
    for k in keys {
        let v = &meta[k];
        match v {
            Value::Null => continue,
            Value::String(s) if s.is_empty() => continue,
            Value::Array(a) if a.is_empty() => continue,
            Value::Number(n) => lines.push(format!("{k}: {n}")),
            Value::Array(_) | Value::Object(_) => lines.push(format!("{k}: {v}")),
            Value::String(s) => {
                let s = one_line(s);
                // Quote what would read back as another type (a number, JSON, a comment).
                let quote =
                    is_int(&s) || s == "null" || s == "~" || starts_json(&s) || has_ws_hash(&s);
                lines.push(format!(
                    "{k}: {}",
                    if quote {
                        Value::String(s).to_string()
                    } else {
                        s
                    }
                ));
            }
            Value::Bool(b) => lines.push(format!("{k}: {b}")),
        }
    }
    format!("---\n{}\n---\n", lines.join("\n"))
}

/// `/\s#/`
fn has_ws_hash(s: &str) -> bool {
    let mut prev_ws = false;
    for c in s.chars() {
        if c == '#' && prev_ws {
            return true;
        }
        prev_ws = is_ws(c);
    }
    false
}

// ------------------------------------------------------------------ resource / todo lines

/// Strip markdown noise from a URL's surroundings: trailing punctuation, a closing paren run.
pub fn clean_url(url: &str) -> String {
    let mut u = url
        .trim_end_matches(['.', ',', ';', ':', '!', '?', '\'', '"'])
        .to_string();
    while u.ends_with(')') && u.matches('(').count() < u.matches(')').count() {
        u.pop();
    }
    u
}

/// The identity of a resource for de-duplication and `dismissed`: its URL, else its path.
pub fn resource_key(
    kind: Option<&str>,
    url: Option<&str>,
    path: Option<&str>,
    text: &str,
) -> String {
    if kind == Some("Git") {
        // One per brief: replaced in place by the server, dismissed as a whole.
        return "git".into();
    }
    if let Some(u) = url.filter(|u| !u.is_empty()) {
        let u = u.split('#').next().unwrap_or("");
        return u.trim_end_matches('/').to_string();
    }
    if let Some(p) = path.filter(|p| !p.is_empty()) {
        return match kind {
            Some("Branch") => format!("branch:{p}"),
            Some("Worktree") => format!("worktree:{p}"),
            _ => p.to_string(),
        };
    }
    text.trim().to_lowercase()
}

/// `/https?:\/\/[^\s<>"'`)\]]+/` — the first bare URL.
fn find_url(s: &str) -> Option<&str> {
    let stop = |c: char| is_ws(c) || "<>\"'`)]".contains(c);
    let mut from = 0;
    while let Some(off) = s[from..].find("http") {
        let i = from + off;
        let rest = &s[i + 4..];
        let after = rest
            .strip_prefix("s://")
            .or_else(|| rest.strip_prefix("://"));
        if let Some(a) = after {
            let n = a.find(stop).unwrap_or(a.len());
            if n > 0 {
                let end = s.len() - a.len() + n;
                return Some(&s[i..end]);
            }
        }
        from = i + 1;
    }
    None
}

/// `/\[([^\]]*)\]\((https?:\/\/[^)\s]+)\)/` → (label, url).
fn find_link(s: &str) -> Option<(&str, &str)> {
    let mut from = 0;
    while let Some(off) = s[from..].find('[') {
        let i = from + off;
        if let Some(close) = s[i + 1..].find(']') {
            let label = &s[i + 1..i + 1 + close];
            let rest = &s[i + 1 + close + 1..];
            if let Some(r) = rest.strip_prefix('(') {
                let after = r
                    .strip_prefix("https://")
                    .map(|a| (a, 8))
                    .or_else(|| r.strip_prefix("http://").map(|a| (a, 7)));
                if let Some((a, scheme)) = after {
                    let n = a.find(|c: char| c == ')' || is_ws(c)).unwrap_or(a.len());
                    if n > 0 && a[n..].starts_with(')') {
                        return Some((label, &r[..scheme + n]));
                    }
                }
            }
        }
        from = i + 1;
    }
    None
}

/// `` /`([^`]+)`/ ``
fn find_code_span(s: &str) -> Option<&str> {
    let mut from = 0;
    while let Some(off) = s[from..].find('`') {
        let i = from + off;
        if let Some(n) = s[i + 1..].find('`')
            && n > 0
        {
            return Some(&s[i + 1..i + 1 + n]);
        }
        from = i + 1;
    }
    None
}

/// `/^([A-Za-z][A-Za-z ]{0,15}?)\s*:\s+(.*)$/` → (kind candidate, value).
fn kind_prefix(t: &str) -> Option<(&str, &str)> {
    let chars: Vec<(usize, char)> = t.char_indices().collect();
    if !chars.first()?.1.is_ascii_alphabetic() {
        return None;
    }
    for k in 1..=16.min(chars.len()) {
        if k > 1 {
            let c = chars[k - 1].1;
            if !(c.is_ascii_alphabetic() || c == ' ') {
                return None;
            }
        }
        let cut = chars.get(k).map_or(t.len(), |x| x.0);
        let rest = t[cut..].trim_start_matches(is_ws);
        if let Some(r) = rest.strip_prefix(':')
            && r.starts_with(is_ws)
        {
            return Some((&t[..cut], r.trim_start_matches(is_ws)));
        }
    }
    None
}

fn canonical_kind(s: &str) -> Option<&'static str> {
    RESOURCE_KINDS
        .iter()
        .copied()
        .find(|k| k.eq_ignore_ascii_case(s))
}

/// The kind a (cleaned) URL classifies as: GitHub PR / issue, claude.ai artifact, else Link.
pub fn classify_url_kind(url: &str) -> &'static str {
    let rest = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"));
    let Some(rest) = rest else { return "Link" };
    let seg_ok = |s: &str| {
        !s.is_empty()
            && s.chars()
                .all(|c| c.is_ascii_alphanumeric() || "_.-".contains(c))
    };
    if let Some(gh) = rest.strip_prefix("github.com/") {
        let parts: Vec<&str> = gh.splitn(5, '/').collect();
        if parts.len() >= 4 && seg_ok(parts[0]) && seg_ok(parts[1]) {
            let digits = parts[3].bytes().take_while(u8::is_ascii_digit).count();
            if digits > 0 {
                match parts[2] {
                    "pull" => return "PR",
                    "issues" => return "Issue",
                    _ => {}
                }
            }
        }
    }
    if let Some(a) = rest.strip_prefix("claude.ai/") {
        let a = a.strip_prefix("code/").unwrap_or(a);
        let a = a
            .strip_prefix("artifacts/")
            .or_else(|| a.strip_prefix("artifact/"));
        if let Some(id) = a
            && id
                .chars()
                .take_while(|c| c.is_ascii_alphanumeric() || *c == '-')
                .count()
                >= 6
        {
            return "Artifact";
        }
    }
    "Link"
}

/// One Resources bullet (without the `- `).
pub fn parse_resource_line(text: &str) -> Resource {
    let t = text.trim();
    let mut kind: Option<String> = None;
    let mut value = t;
    if let Some((k, v)) = kind_prefix(t)
        && let Some(c) = canonical_kind(k)
    {
        kind = Some(c.to_string());
        value = v.trim();
    }
    if kind.as_deref() == Some("Git") {
        let (branch, path, linked) = parse_git_value(value);
        return Resource {
            key: resource_key(kind.as_deref(), None, path.as_deref(), t),
            kind,
            label: branch.clone().or_else(|| path.clone()),
            url: None,
            path,
            text: t.to_string(),
            branch,
            linked,
        };
    }
    let mut url = None;
    let mut label = None;
    let mut path = None;
    if let Some((l, u)) = find_link(value) {
        let l = l.trim();
        label = (!l.is_empty()).then(|| l.to_string());
        url = Some(u.to_string());
    } else if let Some(u) = find_url(value) {
        url = Some(clean_url(u));
    }
    if url.is_none() {
        let p = find_code_span(value).unwrap_or(value).trim();
        path = (!p.is_empty()).then(|| p.to_string());
        label = path.clone();
    }
    if kind.is_none() {
        kind = url.as_deref().map(|u| classify_url_kind(u).to_string());
    }
    let key = resource_key(kind.as_deref(), url.as_deref(), path.as_deref(), t);
    Resource {
        kind,
        label: label.or_else(|| url.clone()),
        url,
        path,
        text: t.to_string(),
        branch: None,
        linked: None,
        key,
    }
}

/// A `Git:` value — `` `branch` · worktree `~/path` `` (a linked worktree) or `` · repo `~/path` ``
/// (the main checkout), `detached` in place of the branch. → (branch, path, linked).
fn parse_git_value(value: &str) -> (Option<String>, Option<String>, Option<bool>) {
    // `/(?:^|\s)(worktree|repo)\s+`([^`]+)`/i`
    let mut root: Option<(usize, bool, &str)> = None;
    for (i, _) in value.char_indices() {
        if i > 0 && !value[..i].ends_with(is_ws) {
            continue;
        }
        let rest = &value[i..];
        let word = ["worktree", "repo"].into_iter().find(|w| {
            rest.get(..w.len())
                .is_some_and(|h| h.eq_ignore_ascii_case(w))
        });
        let Some(w) = word else { continue };
        let after = &rest[w.len()..];
        let span = after.trim_start_matches(is_ws);
        if span.len() == after.len() {
            continue;
        }
        if let Some(inner) = span.strip_prefix('`')
            && let Some(n) = inner.find('`')
            && n > 0
        {
            root = Some((i, w == "worktree", &inner[..n]));
            break;
        }
    }
    // `/^`([^`]+)`/`, before the root
    let branch = value
        .strip_prefix('`')
        .and_then(|r| r.find('`').filter(|n| *n > 0).map(|n| &r[..n]))
        .filter(|_| root.is_none_or(|(i, _, _)| i > 0))
        .map(str::trim)
        .filter(|b| !b.is_empty())
        .map(String::from);
    let path = match root {
        Some((_, _, p)) => p.trim(),
        None if branch.is_some() => "",
        None => value.trim(),
    };
    (
        branch,
        (!path.is_empty()).then(|| path.to_string()),
        root.map(|(_, linked, _)| linked),
    )
}

/// `/^\s*[-*+]\s+(.*)$/`
pub(crate) fn bullet(line: &str) -> Option<&str> {
    let l = line.trim_start_matches(is_ws);
    let r = l.strip_prefix(['-', '*', '+'])?;
    if !r.starts_with(is_ws) {
        return None;
    }
    Some(r.trim_start_matches(is_ws))
}

/// `/^\s*[-*+]\s+\[([ xX])\]\s+(.*)$/`
pub fn parse_todo_line(line: &str) -> Option<Todo> {
    let r = bullet(line)?;
    let r = r.strip_prefix('[')?;
    let mark = r.chars().next()?;
    if !matches!(mark, ' ' | 'x' | 'X') {
        return None;
    }
    let r = r[1..].strip_prefix(']')?;
    if !r.starts_with(is_ws) {
        return None;
    }
    Some(Todo {
        done: mark != ' ',
        text: r.trim().to_string(),
    })
}

/// `/^##\s+(.+?)\s*#*\s*$/` → the heading text, trimmed.
pub(crate) fn heading(line: &str) -> Option<String> {
    let r = line.strip_prefix("##")?;
    if !r.starts_with(is_ws) {
        return None;
    }
    let body = r.trim_start_matches(is_ws);
    if body.is_empty() {
        // `.+?` takes one of the spaces `\s+` gave back: an empty heading.
        return Some(String::new());
    }
    let tail_ok = |s: &str| {
        let s = s.trim_start_matches(is_ws);
        let s = s.trim_start_matches('#');
        s.trim_start_matches(is_ws).is_empty()
    };
    let mut cuts: Vec<usize> = body.char_indices().map(|(i, _)| i).skip(1).collect();
    cuts.push(body.len());
    for cut in cuts {
        if tail_ok(&body[cut..]) {
            return Some(body[..cut].trim().to_string());
        }
    }
    Some(body.trim().to_string())
}

/// `lines.join('\n').replace(/^\s*\n/, '').trimEnd()`
pub(crate) fn join(lines: &[&str]) -> String {
    let s = lines.join("\n");
    let lead = s.len() - s.trim_start_matches(is_ws).len();
    let s = match s[..lead].rfind('\n') {
        Some(nl) => &s[nl + 1..],
        None => &s[..],
    };
    s.trim_end().to_string()
}

// ------------------------------------------------------------------ whole brief

/// Parse a brief (with or without frontmatter). Tolerant: missing sections, any order, extra
/// sections and text, `*` bullets, `[X]`; a legacy `## Plan` reads as `## Todos`.
pub fn parse_brief(text: &str) -> Brief {
    let (meta, body) = parse_frontmatter(text);
    let mut summary: Vec<&str> = Vec::new();
    let mut resources: Vec<&str> = Vec::new();
    let mut todos: Vec<&str> = Vec::new();
    let mut pre: Vec<&str> = Vec::new();
    let mut extra: Vec<(String, Vec<&str>)> = Vec::new();
    #[derive(Clone, Copy)]
    enum Cur {
        Pre,
        Summary,
        Resources,
        Todos,
        Extra(usize),
    }
    let mut cur = Cur::Pre;
    for line in body.split('\n') {
        if let Some(h) = heading(line) {
            cur = match h.to_lowercase().as_str() {
                "summary" => Cur::Summary,
                "resources" => Cur::Resources,
                "todos" | "plan" => Cur::Todos,
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
            Cur::Todos => todos.push(line),
            Cur::Extra(i) => extra[i].1.push(line),
        }
    }
    let resources_text = join(&resources);
    let todos_text = join(&todos);
    let parsed_resources = resources_text
        .split('\n')
        .filter_map(bullet)
        .filter(|b| !b.trim().is_empty())
        .map(parse_resource_line)
        .collect();
    let parsed_todos = todos_text.split('\n').filter_map(parse_todo_line).collect();
    Brief {
        meta,
        preamble: join(&pre),
        summary: join(&summary),
        resources_text,
        todos_text,
        extra: extra
            .into_iter()
            .map(|(heading, lines)| Extra {
                heading,
                body: join(&lines),
            })
            .collect(),
        resources: parsed_resources,
        todos: parsed_todos,
    }
}

/// The body alone: the three sections (and the rest), no frontmatter — what a human edits.
pub fn serialize_body(b: &Brief) -> String {
    let mut out = String::new();
    if !b.preamble.is_empty() {
        out.push_str(&format!("{}\n\n", b.preamble));
    }
    let sec = |name: &str, text: &str| {
        format!(
            "## {name}\n{}",
            if text.is_empty() {
                String::new()
            } else {
                format!("{text}\n")
            }
        )
    };
    out.push_str(&sec("Summary", &b.summary));
    out.push('\n');
    out.push_str(&sec("Resources", &b.resources_text));
    out.push('\n');
    out.push_str(&sec("Todos", &b.todos_text));
    for s in &b.extra {
        out.push('\n');
        out.push_str(&sec(&s.heading, &s.body));
    }
    out
}

/// A parsed (possibly modified) brief → canonical text.
pub fn serialize_brief(b: &Brief) -> String {
    serialize_frontmatter(&b.meta) + &serialize_body(b)
}

pub fn empty_brief(session_id: &str) -> Brief {
    let mut meta = Map::new();
    meta.insert("session".into(), Value::String(session_id.into()));
    parse_brief(&serialize_brief(&Brief {
        meta,
        ..Default::default()
    }))
}

/// Keys that were in `before`'s Resources and are gone from `after`'s: what a human deleted.
pub fn removed_resource_keys(before: &Brief, after: &Brief) -> Vec<String> {
    let now: std::collections::HashSet<&str> =
        after.resources.iter().map(|r| r.key.as_str()).collect();
    before
        .resources
        .iter()
        .map(|r| r.key.clone())
        .filter(|k| !k.is_empty() && !now.contains(k.as_str()))
        .collect()
}

pub fn add_dismissed(dismissed: &[String], keys: &[String]) -> Vec<String> {
    let mut out: Vec<String> = dismissed
        .iter()
        .filter(|k| !keys.contains(k))
        .cloned()
        .collect();
    out.extend(keys.iter().cloned());
    let skip = out.len().saturating_sub(MAX_DISMISSED);
    out.split_off(skip)
}

fn dismissed_of(meta: &Map<String, Value>) -> Vec<String> {
    match meta.get("dismissed") {
        Some(Value::Array(a)) => a
            .iter()
            .map(|v| match v {
                Value::String(s) => s.clone(),
                v => v.to_string(),
            })
            .collect(),
        _ => Vec::new(),
    }
}

/// A human edit, exactly as the server's `PUT`: the edited text is authoritative for the body;
/// the frontmatter stays the stored one (keys the edit adds are kept, it can't change existing
/// ones — the machine keys are the writer's); resource lines the edit removed go to `dismissed`;
/// `editedAt` is stamped. The caller then [`save`]s (which stamps `session` / `updated`).
pub fn human_edit(
    before: &Brief,
    markdown: &str,
    now: &str,
    session_cwd: Option<&str>,
    self_name: &str,
) -> Result<Brief> {
    if markdown.encode_utf16().count() > MAX_EDIT_CHARS {
        return Err(Error::Other(format!(
            "brief too long (max {MAX_EDIT_CHARS} chars)"
        )));
    }
    let incoming = parse_brief(markdown);
    let removed = removed_resource_keys(before, &incoming);
    let mut meta = incoming.meta.clone();
    for (k, v) in &before.meta {
        meta.insert(k.clone(), v.clone());
    }
    let dismissed = add_dismissed(&dismissed_of(&before.meta), &removed);
    meta.insert("dismissed".into(), json!(dismissed));
    meta.insert("editedAt".into(), Value::String(now.into()));
    if let Some(cwd) = session_cwd.filter(|c| !c.is_empty()) {
        meta.insert("cwd".into(), Value::String(cwd.into()));
    }
    if meta.get("host").is_none_or(Value::is_null) {
        meta.insert("host".into(), Value::String(self_name.into()));
    }
    Ok(Brief { meta, ..incoming })
}

// ------------------------------------------------------------------ continue prompt

/// The line that ends the handed-over brief in a continue prompt; the task follows it.
pub const CONTINUE_SEPARATOR: &str = "---";
/// The task when none is given (the server's `CONTINUE_DEFAULT_TASK`).
pub const CONTINUE_DEFAULT_TASK: &str = "Pick up where it left off. Check the current state (git status, the PRs above) before changing anything, and tell me briefly what you found first.";

/// A resource as a continue-prompt bullet: `[label](url)`, a bare url, else its label / text.
fn prompt_link(r: &Resource) -> String {
    match (&r.url, &r.label) {
        (Some(u), Some(l)) if l != u => format!("[{}]({u})", l.replace(['[', ']'], "")),
        (Some(u), _) => u.clone(),
        (None, l) => l.clone().unwrap_or_else(|| r.text.clone()),
    }
}

/// The first prompt for a NEW session that picks up where the brief's session left off — the
/// server's `continuePrompt(brief, { host, cwd })`; `None` falls back to the frontmatter. Where
/// it ran, the summary, Origin, PRs and the linked worktree, then the separator line the task
/// goes below.
pub fn continue_prompt(brief: &Brief, host: Option<&str>, cwd: Option<&str>) -> String {
    let host = host.map(String::from).or_else(|| brief.meta_str("host"));
    let cwd = cwd.map(String::from).or_else(|| brief.meta_str("cwd"));
    let session = brief.meta_str("session");
    let origin = [
        Some(match &session {
            Some(s) => format!("session {s}"),
            None => "a previous session".into(),
        }),
        host.map(|h| format!("on {h}")),
        cwd.map(|c| format!("in {c}")),
    ]
    .into_iter()
    .flatten()
    .collect::<Vec<_>>()
    .join(" ");
    let list = |kind: &str| {
        brief
            .resources
            .iter()
            .filter(|r| r.kind.as_deref() == Some(kind))
            .map(|r| format!("- {}", prompt_link(r)))
            .collect::<Vec<_>>()
    };
    let mut out = vec![format!("Continue the work of {origin}. Its brief:")];
    if !brief.summary.is_empty() {
        out.push(brief.summary.clone());
    }
    let origins = list("Origin");
    if !origins.is_empty() {
        out.push(format!("Origin:\n{}", origins.join("\n")));
    }
    let prs = list("PR");
    if !prs.is_empty() {
        out.push(format!("PRs:\n{}", prs.join("\n")));
    }
    let wt = brief.resources.iter().find(|r| {
        r.path.is_some()
            && match r.kind.as_deref() {
                Some("Git") => r.linked == Some(true),
                Some("Worktree") => true,
                _ => false,
            }
    });
    if let Some(r) = wt {
        let branch = match (&r.branch, r.kind.as_deref()) {
            (Some(b), Some("Git")) => format!(" (branch `{b}`)"),
            _ => String::new(),
        };
        out.push(format!(
            "Worktree: `{}`{branch}",
            r.path.as_deref().unwrap_or_default()
        ));
    }
    out.push(CONTINUE_SEPARATOR.into());
    out.join("\n\n")
}

// ------------------------------------------------------------------ the JSON view

/// Where the brief's session is, for the view and the continue prompt.
pub struct Where<'a> {
    /// The name rows are tagged with (the caller's name for this host).
    pub host_label: &'a str,
    /// This machine's config `self` (what the server puts in the prompt).
    pub self_name: &'a str,
    /// The live session's cwd, if it is live.
    pub cwd: Option<&'a str>,
    /// The session's directory, absolute (no `~`), when known.
    pub abs_cwd: Option<&'a str>,
    /// The git checkout root (repo or linked worktree) containing it, absolute.
    pub git_root: Option<&'a str>,
}

/// `fleet brief --json`: the web API's GET shape (`generating` / `enabled` and the editor link
/// are the server's to know and left out), plus `body` (the markdown without frontmatter) and
/// `path`. `parsed.plan` is a deprecated alias of `parsed.todos` (one release).
pub fn view(id: &str, brief: &Brief, exists: bool, w: &Where, path: &Path) -> Value {
    let host = brief.meta_str("host").unwrap_or_else(|| w.self_name.into());
    let cwd = w.cwd.map(String::from).or_else(|| brief.meta_str("cwd"));
    let get = |k: &str| brief.meta.get(k).cloned().unwrap_or(Value::Null);
    json!({
        "host": w.host_label,
        "id": id,
        "exists": exists,
        "markdown": serialize_brief(brief),
        "body": serialize_body(brief),
        "parsed": {
            "summary": brief.summary,
            "resources": brief.resources.iter().map(|r| json!({
                "kind": r.kind, "label": r.label, "url": r.url, "path": r.path, "text": r.text,
                "branch": r.branch, "linked": r.linked,
            })).collect::<Vec<_>>(),
            "todos": brief.todos,
            "plan": brief.todos,
        },
        "absCwd": w.abs_cwd,
        "gitRoot": w.git_root,
        "updated": get("updated"),
        "editedAt": get("editedAt"),
        "generatedAt": get("generatedAt"),
        "generatedThrough": brief.meta.get("generatedThrough").cloned().unwrap_or(json!(0)),
        "continuePrompt": continue_prompt(brief, Some(&host), cwd.as_deref()),
        "path": path.display().to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    // Shared with web/tests/briefs.test.mjs — regenerate with `node testdata/briefs/gen.mjs`.
    const SAMPLE: &str = include_str!("../../../../testdata/briefs/sample.md");
    const CANONICAL: &str = include_str!("../../../../testdata/briefs/sample.canonical.md");
    const PARSED: &str = include_str!("../../../../testdata/briefs/sample.parsed.json");
    const CONTINUE: &str = include_str!("../../../../testdata/briefs/sample.continue.txt");
    const EDIT: &str = include_str!("../../../../testdata/briefs/edit.md");
    const EDITED: &str = include_str!("../../../../testdata/briefs/sample.edited.md");
    const EDIT_FM: &str = include_str!("../../../../testdata/briefs/edit-frontmatter.md");
    const EDITED_FM: &str =
        include_str!("../../../../testdata/briefs/sample.edited-frontmatter.md");
    const EMPTY_EDITED: &str = include_str!("../../../../testdata/briefs/empty.edited.md");
    const SID: &str = "aaaaaaaa-1111-2222-3333-444444444444";
    const NOW: &str = "2026-01-02T05:00:00.000Z";

    fn edit(before: &Brief, text: &str) -> String {
        let mut b = human_edit(before, text, NOW, Some("~/Code/project"), "laptop").unwrap();
        b.meta.insert("session".into(), json!(SID));
        b.meta.insert("updated".into(), json!(NOW));
        serialize_brief(&b)
    }

    #[test]
    fn shared_fixture_parses_like_the_server() {
        let b = parse_brief(SAMPLE);
        let want: Value = serde_json::from_str(PARSED).unwrap();
        let got = serde_json::to_value(&b).unwrap();
        assert_eq!(got, want);
        // Key order in the frontmatter is part of the contract (unknown keys as found).
        let keys: Vec<&String> = b.meta.keys().collect();
        let want_keys: Vec<&String> = want["meta"].as_object().unwrap().keys().collect();
        assert_eq!(keys, want_keys);
    }

    #[test]
    fn shared_fixture_round_trips_to_the_canonical_form() {
        let b = parse_brief(SAMPLE);
        assert_eq!(serialize_brief(&b), CANONICAL);
        // Canonical is a fixed point, unknown keys and all.
        assert_eq!(serialize_brief(&parse_brief(CANONICAL)), CANONICAL);
        assert_eq!(b.meta["zeta"], json!("kept as is"));
        assert_eq!(b.meta["alpha"], json!({"nested": [1, 2], "b": "x"}));
        assert_eq!(b.meta["quoted"], json!("123"));
    }

    #[test]
    fn continue_prompt_matches_the_server_byte_for_byte() {
        assert_eq!(continue_prompt(&parse_brief(SAMPLE), None, None), CONTINUE);
        let empty = continue_prompt(&empty_brief(SID), Some("workstation"), None);
        assert_eq!(
            empty,
            format!("Continue the work of session {SID} on workstation. Its brief:\n\n---")
        );
    }

    #[test]
    fn human_edit_matches_the_server_put() {
        let before = parse_brief(SAMPLE);
        assert_eq!(edit(&before, EDIT), EDITED);
        // An edit that carries frontmatter: new keys kept (first), machine keys stay ours.
        assert_eq!(edit(&before, EDIT_FM), EDITED_FM);
        // First write: no dismissed list, host defaults to self.
        assert_eq!(edit(&empty_brief(SID), EDIT), EMPTY_EDITED);
    }

    #[test]
    fn deleted_resource_lines_become_dismissed() {
        let before = parse_brief(
            "## Resources\n- PR: [o/r#12](https://github.com/o/r/pull/12)\n- hand-written line\n- File: `src/a.ts`\n",
        );
        let after = parse_brief("## Resources\n- hand-written line\n- File: `src/a.ts`\n");
        assert_eq!(
            removed_resource_keys(&before, &after),
            ["https://github.com/o/r/pull/12"]
        );
        let s = |v: &[&str]| v.iter().map(|x| x.to_string()).collect::<Vec<_>>();
        assert_eq!(
            add_dismissed(&s(&["a", "b"]), &s(&["a", "c"])),
            s(&["b", "a", "c"])
        );
        let many: Vec<String> = (0..250).map(|i| i.to_string()).collect();
        let out = add_dismissed(&many, &s(&["x"]));
        assert_eq!(out.len(), MAX_DISMISSED);
        assert_eq!(out.last().unwrap(), "x");
    }

    #[test]
    fn tolerant_parsing_and_the_skeleton() {
        let b = parse_brief(
            "Some preamble\n\n## plan\n* [X] Done it\n* [ ] next\nnot a checkbox\n\n## Notes\nfree text\n\n## Summary\nhello\n",
        );
        assert!(b.meta.is_empty());
        assert_eq!(b.summary, "hello");
        assert_eq!(
            b.todos,
            [
                Todo {
                    done: true,
                    text: "Done it".into()
                },
                Todo {
                    done: false,
                    text: "next".into()
                }
            ]
        );
        assert_eq!(
            serialize_brief(&empty_brief(SID)),
            format!("---\nsession: {SID}\n---\n## Summary\n\n## Resources\n\n## Todos\n")
        );
    }

    #[test]
    fn frontmatter_values_that_would_read_back_as_another_type_are_quoted() {
        let mut meta = Map::new();
        for (k, v) in [
            ("session", SID),
            ("cwd", "~/a #b"),
            ("todos", "123"),
            ("note", "[x]"),
        ] {
            meta.insert(k.into(), json!(v));
        }
        let text = serialize_frontmatter(&meta);
        let (back, _) = parse_frontmatter(&text);
        assert_eq!(back, meta);
        assert!(text.contains("todos: \"123\""));
    }

    #[test]
    fn resource_lines_classify_like_the_server() {
        let r = parse_resource_line("see https://github.com/o/r/pull/5.");
        assert_eq!(r.kind.as_deref(), Some("PR"));
        assert_eq!(r.url.as_deref(), Some("https://github.com/o/r/pull/5"));
        assert_eq!(
            parse_resource_line("Worktree: `~/wt/x`").key,
            "worktree:~/wt/x"
        );
        assert_eq!(parse_resource_line("Just Text").key, "Just Text");
        assert_eq!(
            classify_url_kind("https://claude.ai/artifacts/abcdef12"),
            "Artifact"
        );
        assert_eq!(classify_url_kind("https://github.com/o/r/pull/x"), "Link");
        assert_eq!(clean_url("https://x.dev/a_(b))."), "https://x.dev/a_(b)");
    }

    #[test]
    fn git_lines_parse_like_the_server() {
        let r = parse_resource_line("Git: `feat-x` · worktree `~/Code/p/.worktrees/x`");
        assert_eq!(r.kind.as_deref(), Some("Git"));
        assert_eq!(r.branch.as_deref(), Some("feat-x"));
        assert_eq!(r.path.as_deref(), Some("~/Code/p/.worktrees/x"));
        assert_eq!(r.linked, Some(true));
        assert_eq!(r.label.as_deref(), Some("feat-x"));
        assert_eq!(r.key, "git");
        let d = parse_resource_line("Git: detached · repo `~/Code/p`");
        assert_eq!(
            (d.branch, d.path.as_deref(), d.linked, d.label.as_deref()),
            (None, Some("~/Code/p"), Some(false), Some("~/Code/p"))
        );
        let hand = parse_resource_line("git: `main` — merged, safe to delete");
        assert_eq!(
            (hand.kind.as_deref(), hand.branch.as_deref(), hand.path),
            (Some("Git"), Some("main"), None)
        );
        let other = parse_resource_line("File: `src/a.ts`");
        assert_eq!((other.branch, other.linked), (None, None));
        // A legacy `## Plan` reads as Todos and is written back as `## Todos`.
        let b = parse_brief("## Plan\n- [ ] a\n");
        assert_eq!(b.todos.len(), 1);
        assert!(serialize_brief(&b).ends_with("## Todos\n- [ ] a\n"));
    }

    #[test]
    fn session_ids_and_atomic_writes() {
        assert!(is_session_id(SID));
        for bad in ["", "-x", "../x", "a/b", "a.b", &"a".repeat(129)] {
            assert!(!is_session_id(bad), "{bad}");
        }
        let d = tempfile::tempdir().unwrap();
        let dir = d.path().join("briefs");
        assert_eq!(read(&dir, SID).unwrap(), None);
        write(&dir, SID, "x").unwrap();
        write(&dir, SID, "y").unwrap();
        assert_eq!(read(&dir, SID).unwrap().as_deref(), Some("y"));
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(file(&dir, SID).unwrap())
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600);
        // No temp files left behind.
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 1);
    }
}
