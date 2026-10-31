//! `fleet diff`: a live session's Changes. Which checkouts it may work in (its cwd's, and those
//! holding the files its transcripts show it edited) and, per checkout, what its working tree
//! changed against its branch point, as file paths and line counts. Facts only: which of the
//! candidates a consumer counts is its own rule.
//!
//! The working tree is snapshotted as upstream's web diff reviewer does it (`web/lib/diff.mjs`):
//! a throwaway copy of the index, `add -A` into it, `write-tree`. Nothing in the repo changes but
//! loose objects, and no hook, fsmonitor or lazy fetch runs (every git call is fenced, [`git`]).
//! The edited files are read incrementally: per session, the checkouts found so far and how far
//! each transcript was read live in `${XDG_STATE_HOME:-~/.local/state}/fleet/diff/<id>.json`.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs::{self, File, OpenOptions};
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom};
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant, SystemTime};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::core::harness::Harness;

/// Candidates read at most: the cwd's, then the most recently edited.
pub const MAX_CANDIDATES: usize = 12;
/// Files listed per checkout; `more_files` counts the rest.
pub const MAX_FILES: usize = 500;
const GIT_TIMEOUT: Duration = Duration::from_secs(20);
/// The whole command stops here; checkouts not yet read are `skipped: timeout`.
const TOTAL_TIMEOUT: Duration = Duration::from_secs(50);
const LOCK_WAIT: Duration = Duration::from_secs(10);
const STATE_TTL: Duration = Duration::from_secs(7 * 24 * 3600);
/// Checkouts snapshotted at once.
const WORKERS: usize = 2;
/// Claude Code tools whose inputs name a file they write.
const EDIT_TOOLS: [&str; 4] = ["Edit", "MultiEdit", "Write", "NotebookEdit"];

pub struct Input {
    pub session_id: Option<String>,
    pub harness: Harness,
    pub cwd: Option<String>,
    /// The session's transcript and its subagents' (Claude Code); empty for Codex.
    pub transcripts: Vec<PathBuf>,
    pub since: Option<String>,
}

/// The answer (contract C2), without `ok`'s siblings the CLI adds.
pub fn run(input: &Input) -> Value {
    let deadline = Instant::now() + TOTAL_TIMEOUT;
    let home = dirs::home_dir().unwrap_or_default();
    let edited = match (&input.session_id, input.harness) {
        (Some(id), Harness::Claude) => edited_checkouts(id, &input.transcripts, &home, deadline),
        _ => Vec::new(),
    };
    let cwd_root = input
        .cwd
        .as_deref()
        .and_then(|c| checkout_from(Path::new(c), &home));
    let candidates = candidates(cwd_root, edited);

    let outcomes = in_parallel(&candidates, |c| {
        if Instant::now() >= deadline {
            return Err("timeout");
        }
        snapshot(&c.root, deadline)
    });
    let mut rows: Vec<(Candidate, Result<Snap, &'static str>)> =
        candidates.into_iter().zip(outcomes).collect();
    let fp = fingerprint(&rows);
    if input.since.as_deref() == Some(fp.as_str()) {
        return json!({ "fingerprint": fp, "unchanged": true });
    }
    let diffs = in_parallel(&rows, |(_, snap)| match snap {
        Ok(s) if Instant::now() < deadline => files(s, deadline).map(Some),
        Ok(_) => Err("timeout"),
        Err(_) => Ok(None),
    });
    let mut failed = false;
    let mut out_candidates = Vec::new();
    let mut skipped = Vec::new();
    for ((cand, snap), diff) in rows.iter_mut().zip(diffs) {
        let result = match (snap.as_ref(), diff) {
            (Ok(s), Ok(Some(d))) => Ok((s, d)),
            (Ok(_), Err(detail)) => {
                failed = true;
                *snap = Err(detail);
                Err(detail)
            }
            (Err(detail), _) => Err(*detail),
            (Ok(_), Ok(None)) => unreachable!("a snapshot always gets its files"),
        };
        match result {
            Ok((s, d)) => out_candidates.push(candidate_json(cand, s, d)),
            Err(detail) => skipped.push(json!({
                "root": cand.root.display().to_string(),
                "why": cand.why,
                "detail": detail,
            })),
        }
    }
    // A checkout whose diff failed after its snapshot: the fingerprint says so too, so the next
    // `--since` can't answer "unchanged" to an answer that skipped it.
    let fp = if failed { fingerprint(&rows) } else { fp };
    json!({
        "fingerprint": fp,
        "unchanged": false,
        "candidates": out_candidates,
        "skipped": skipped,
    })
}

// ---------------------------------------------------------------------------------------------
// Candidates (contract C3)

#[derive(Debug, Clone, PartialEq, Eq)]
struct Candidate {
    root: PathBuf,
    why: Vec<&'static str>,
}

/// The cwd's checkout first, then edited ones by most recent edit, at most [`MAX_CANDIDATES`].
fn candidates(cwd_root: Option<PathBuf>, edited: Vec<PathBuf>) -> Vec<Candidate> {
    let mut out: Vec<Candidate> = Vec::new();
    if let Some(root) = cwd_root {
        let why = if edited.contains(&root) {
            vec!["cwd", "edited"]
        } else {
            vec!["cwd"]
        };
        out.push(Candidate { root, why });
    }
    for root in edited {
        if out.len() >= MAX_CANDIDATES {
            break;
        }
        if !out.iter().any(|c| c.root == root) {
            out.push(Candidate {
                root,
                why: vec!["edited"],
            });
        }
    }
    out
}

/// The checkout holding `dir` (or the nearest existing ancestor): the closest directory with a
/// `.git` (a dir, or a linked worktree's file). Walked on the real path, symlinks resolved, as
/// git does. Never `home` or `/`.
pub(crate) fn checkout_from(dir: &Path, home: &Path) -> Option<PathBuf> {
    let real = dir.ancestors().find_map(|d| fs::canonicalize(d).ok())?;
    let home = fs::canonicalize(home).unwrap_or_else(|_| home.to_path_buf());
    for d in real.ancestors() {
        if d == home || d == Path::new("/") || d.as_os_str().is_empty() {
            return None;
        }
        if d.join(".git").exists() {
            return Some(d.to_path_buf());
        }
    }
    None
}

/// Per session: the checkouts its edits are in, with the latest edit's timestamp, and how far
/// each transcript was read.
#[derive(Debug, Default, Serialize, Deserialize)]
struct State {
    #[serde(default)]
    transcripts: BTreeMap<String, Mark>,
    #[serde(default)]
    checkouts: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
struct Mark {
    ino: u64,
    offset: u64,
}

pub(crate) fn state_dir() -> PathBuf {
    if let Some(d) = std::env::var_os("FLEET_DIFF_STATE_DIR").filter(|d| !d.is_empty()) {
        return PathBuf::from(d);
    }
    std::env::var_os("XDG_STATE_HOME")
        .filter(|d| !d.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| dirs::home_dir().unwrap_or_default().join(".local/state"))
        .join("fleet/diff")
}

/// The checkouts the session's transcripts show it edited, most recent edit first. Reads only
/// what each transcript grew by since the last call, under a per-session lock; a call that can't
/// get the lock (or write the state) still answers, from a full read it doesn't save.
fn edited_checkouts(
    session_id: &str,
    transcripts: &[PathBuf],
    home: &Path,
    deadline: Instant,
) -> Vec<PathBuf> {
    let dir = state_dir();
    let safe: String = session_id
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect();
    let state_path = dir.join(format!("{safe}.json"));
    let lock = fs::create_dir_all(&dir)
        .ok()
        .and_then(|_| lock(&dir.join(format!("{safe}.lock"))));
    let mut state = if lock.is_some() {
        fs::read(&state_path)
            .ok()
            .and_then(|b| serde_json::from_slice::<State>(&b).ok())
            .unwrap_or_default()
    } else {
        State::default()
    };
    let mut memo = HashMap::new();
    for t in transcripts {
        let key = t.display().to_string();
        let mark = state.transcripts.get(&key).copied();
        if Instant::now() >= deadline {
            break;
        }
        if let Some(m) = read_edits(t, mark, home, deadline, &mut memo, &mut state.checkouts) {
            state.transcripts.insert(key, m);
        }
    }
    if lock.is_some() {
        save(&state_path, &state);
        prune(&dir);
    }
    drop(lock);
    let mut roots: Vec<(String, String)> = state.checkouts.into_iter().collect();
    // Most recent edit first; equal times by path, so the order is stable.
    roots.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    roots.into_iter().map(|(r, _)| PathBuf::from(r)).collect()
}

/// An exclusive `flock` on `path`, waiting up to [`LOCK_WAIT`]; released when dropped. A lock
/// whose file [`prune`] unlinked meanwhile is retried on the file now at `path`.
fn lock(path: &Path) -> Option<File> {
    let started = Instant::now();
    loop {
        let f = OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(path)
            .ok()?;
        if try_lock(&f) {
            if still_at(&f, path) {
                return Some(f);
            }
            continue;
        }
        if started.elapsed() >= LOCK_WAIT {
            return None;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn try_lock(f: &File) -> bool {
    use std::os::fd::AsRawFd;
    // SAFETY: flock on a descriptor the caller owns.
    unsafe { libc::flock(f.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) == 0 }
}

/// Whether `f` is still the file at `path` (not unlinked or replaced since it was opened).
fn still_at(f: &File, path: &Path) -> bool {
    match (f.metadata(), fs::metadata(path)) {
        (Ok(a), Ok(b)) => a.dev() == b.dev() && a.ino() == b.ino(),
        _ => false,
    }
}

fn save(path: &Path, state: &State) {
    let Ok(bytes) = serde_json::to_vec(state) else {
        return;
    };
    let tmp = path.with_extension(format!("json.tmp-{}", std::process::id()));
    if fs::write(&tmp, bytes).is_ok() && fs::rename(&tmp, path).is_err() {
        let _ = fs::remove_file(&tmp);
    }
}

/// Drop state files no call has touched for [`STATE_TTL`]: their sessions are long gone. Only
/// under the session's own lock, so a call still holding it (a session resumed after a week)
/// keeps its state.
fn prune(dir: &Path) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    let now = SystemTime::now();
    for e in entries.flatten() {
        let p = e.path();
        if p.extension().is_some_and(|x| x == "json")
            && let Ok(m) = e.metadata().and_then(|m| m.modified())
            && now.duration_since(m).is_ok_and(|age| age > STATE_TTL)
        {
            let lock_path = p.with_extension("lock");
            let held = OpenOptions::new()
                .create(true)
                .truncate(false)
                .write(true)
                .open(&lock_path)
                .ok()
                .filter(|f| try_lock(f) && still_at(f, &lock_path));
            if held.is_some() {
                let _ = fs::remove_file(&p);
                let _ = fs::remove_file(&lock_path);
            }
        }
    }
}

/// Read a transcript from `mark` (from the start when it's new, replaced or shorter), adding
/// each edited file's checkout with its latest timestamp. Only complete lines are read: a
/// partial last line waits for the next call. `None` when the file can't be read.
fn read_edits(
    path: &Path,
    mark: Option<Mark>,
    home: &Path,
    deadline: Instant,
    memo: &mut HashMap<PathBuf, Option<PathBuf>>,
    checkouts: &mut BTreeMap<String, String>,
) -> Option<Mark> {
    let mut f = File::open(path).ok()?;
    let meta = f.metadata().ok()?;
    let start = match mark {
        Some(m) if m.ino == meta.ino() && m.offset <= meta.len() => m.offset,
        _ => 0,
    };
    f.seek(SeekFrom::Start(start)).ok()?;
    let mut reader = BufReader::new(f);
    let mut offset = start;
    let mut line = Vec::new();
    loop {
        line.clear();
        let n = reader.read_until(b'\n', &mut line).ok()?;
        // Past the deadline the read stops at a line's end: the next call goes on from there.
        if n == 0 || line.last() != Some(&b'\n') || Instant::now() >= deadline {
            break;
        }
        offset += n as u64;
        let text = String::from_utf8_lossy(&line);
        for (file, ts) in edits_in_line(&text, home) {
            let dir = file.parent().map(Path::to_path_buf).unwrap_or_default();
            let root = memo
                .entry(dir.clone())
                .or_insert_with(|| checkout_from(&dir, home))
                .clone();
            if let Some(root) = root {
                let key = root.display().to_string();
                let latest = checkouts.entry(key).or_default();
                if ts > *latest {
                    *latest = ts;
                }
            }
        }
    }
    Some(Mark {
        ino: meta.ino(),
        offset,
    })
}

/// The files one Claude Code transcript line shows edited, each with the line's timestamp:
/// [`EDIT_TOOLS`] inputs and a `Bash` command's write targets ([`shell_writes`]).
fn edits_in_line(line: &str, home: &Path) -> Vec<(PathBuf, String)> {
    if !line.contains("\"tool_use\"") {
        return Vec::new();
    }
    let Ok(v) = serde_json::from_str::<Value>(line) else {
        return Vec::new();
    };
    let ts = v["timestamp"].as_str().unwrap_or_default().to_string();
    let mut out = Vec::new();
    for block in v["message"]["content"].as_array().into_iter().flatten() {
        if block["type"] != "tool_use" {
            continue;
        }
        let name = block["name"].as_str().unwrap_or_default();
        let input = &block["input"];
        let mut paths: Vec<PathBuf> = Vec::new();
        if EDIT_TOOLS.contains(&name) {
            for k in ["file_path", "notebook_path"] {
                if let Some(p) = input[k].as_str().and_then(|p| absolute(p, home)) {
                    paths.push(p);
                }
            }
            for e in input["edits"].as_array().into_iter().flatten() {
                if let Some(p) = e["file_path"].as_str().and_then(|p| absolute(p, home)) {
                    paths.push(p);
                }
            }
        } else if name == "Bash"
            && let Some(cmd) = input["command"].as_str()
        {
            paths.extend(shell_writes(cmd, home));
        }
        out.extend(paths.into_iter().map(|p| (p, ts.clone())));
    }
    out
}

/// `~/x` → `home/x`; an absolute path as it is (never under `/dev` or `/proc`); else `None`.
fn absolute(p: &str, home: &Path) -> Option<PathBuf> {
    let p = p.trim();
    let abs = if let Some(rest) = p.strip_prefix("~/") {
        home.join(rest)
    } else if p.starts_with('/') {
        PathBuf::from(p)
    } else {
        return None;
    };
    let s = abs.to_string_lossy();
    if s.starts_with("/dev/") || s.starts_with("/proc/") || s.contains('\0') {
        return None;
    }
    Some(abs)
}

/// Files a shell command writes: the targets of `>`, `>>`, `N>` and `tee [-a]`, resolved against
/// the last `cd <dir>` before them when relative (upstream `touched.mjs`, `ingestCommand`). A
/// good-enough read, not a shell parser: `/dev/null`, `&1` and `$VAR` targets are skipped.
pub(crate) fn shell_writes(cmd: &str, home: &Path) -> Vec<PathBuf> {
    let chars: Vec<char> = cmd.chars().collect();
    let mut cds: Vec<(usize, Option<PathBuf>)> = Vec::new();
    let mut targets: Vec<(usize, String)> = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c == 'c' && word_at(&chars, i, "cd") {
            let (tok, _) = token(&chars, i + 2);
            cds.push((i, tok.and_then(|t| absolute(&t, home))));
            i += 2;
            continue;
        }
        if c == 't' && word_at(&chars, i, "tee") {
            let mut j = skip_ws(&chars, i + 3);
            if chars.get(j) == Some(&'-') && chars.get(j + 1) == Some(&'a') {
                j = skip_ws(&chars, j + 2);
            }
            if let (Some(t), _) = token(&chars, j) {
                targets.push((i, t));
            }
            i += 3;
            continue;
        }
        if c == '>' {
            // `N>`: one digit may come before it; the char before that mustn't make it
            // another operator (`<>`, `>&`, `&>`).
            let mut j = i;
            if j > 0 && chars[j - 1].is_ascii_digit() {
                j -= 1;
            }
            let before_ok = j == 0 || !matches!(chars[j - 1], '<' | '>' | '&');
            let mut k = i + 1;
            if chars.get(k) == Some(&'>') {
                k += 1;
            }
            if before_ok && let (Some(t), _) = token(&chars, skip_ws(&chars, k)) {
                targets.push((i, t));
            }
            i = k;
            continue;
        }
        i += 1;
    }
    let mut out = Vec::new();
    for (at, t) in targets {
        if t.is_empty() || t == "/dev/null" || t.starts_with('&') || t.starts_with('$') {
            continue;
        }
        let abs = absolute(&t, home).or_else(|| {
            if t.starts_with('~') {
                return None;
            }
            cds.iter()
                .rev()
                .find(|(pos, _)| *pos < at)
                .and_then(|(_, d)| d.as_ref().map(|d| d.join(&t)))
        });
        if let Some(p) = abs {
            out.push(p);
        }
    }
    out
}

/// `word` at `i`, as a whole word: nothing word-like before it, whitespace after it.
fn word_at(chars: &[char], i: usize, word: &str) -> bool {
    let w: Vec<char> = word.chars().collect();
    if chars.len() < i + w.len() || chars[i..i + w.len()] != w[..] {
        return false;
    }
    let before_ok = i == 0
        || !(chars[i - 1].is_alphanumeric() || matches!(chars[i - 1], '_' | '-' | '.' | '/'));
    let after_ok = chars.get(i + w.len()).is_some_and(|c| c.is_whitespace());
    before_ok && after_ok
}

fn skip_ws(chars: &[char], mut i: usize) -> usize {
    while chars.get(i).is_some_and(|c| *c == ' ' || *c == '\t') {
        i += 1;
    }
    i
}

/// One shell word from `i`: a quoted string (quotes dropped) or a run of non-operator chars.
fn token(chars: &[char], i: usize) -> (Option<String>, usize) {
    let i = skip_ws(chars, i);
    match chars.get(i) {
        Some(q @ ('"' | '\'')) => {
            let end = chars[i + 1..].iter().position(|c| c == q);
            match end {
                Some(n) => (Some(chars[i + 1..i + 1 + n].iter().collect()), i + n + 2),
                None => (None, chars.len()),
            }
        }
        Some(_) => {
            let n = chars[i..]
                .iter()
                .position(|c| {
                    c.is_whitespace() || matches!(c, ';' | '&' | '|' | '<' | '>' | '(' | ')')
                })
                .unwrap_or(chars.len() - i);
            let t: String = chars[i..i + n].iter().collect();
            ((!t.is_empty()).then_some(t), i + n)
        }
        None => (None, i),
    }
}

// ---------------------------------------------------------------------------------------------
// Snapshots and their Changes (contract C4, C6, C7)

#[derive(Debug)]
enum GitError {
    Failed,
    Timeout,
}

impl GitError {
    fn detail(&self) -> &'static str {
        match self {
            GitError::Failed => "git_failed",
            GitError::Timeout => "timeout",
        }
    }
}

/// `git -C <root> …`, fenced: git's own env vars cleared (but `GIT_INDEX_FILE` when `index` is
/// given), no hooks, no fsmonitor, no lazy fetch, no optional locks, no prompt; at most
/// [`GIT_TIMEOUT`] and never past `deadline`. Stdout on exit 0.
fn git(
    root: &Path,
    args: &[&str],
    index: Option<&Path>,
    deadline: Instant,
) -> Result<Vec<u8>, GitError> {
    let mut c = Command::new("git");
    for (k, _) in std::env::vars_os() {
        if k.to_string_lossy().starts_with("GIT_") {
            c.env_remove(k);
        }
    }
    c.env("GIT_NO_LAZY_FETCH", "1")
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_OPTIONAL_LOCKS", "0");
    if let Some(i) = index {
        c.env("GIT_INDEX_FILE", i);
    }
    c.arg("-C").arg(root);
    for cfg in [
        "core.quotePath=false",
        "core.fsmonitor=false",
        "core.hooksPath=/dev/null",
        "core.untrackedCache=false",
        "core.splitIndex=false",
        "fetch.negotiationAlgorithm=noop",
    ] {
        c.args(["-c", cfg]);
    }
    c.args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    let mut child = c.spawn().map_err(|_| GitError::Failed)?;
    let mut stdout = child.stdout.take().expect("piped");
    let reader = std::thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = stdout.read_to_end(&mut buf);
        buf
    });
    let until = deadline.min(Instant::now() + GIT_TIMEOUT);
    let status = loop {
        match child.try_wait() {
            Ok(Some(st)) => break Some(st),
            Ok(None) if Instant::now() >= until => {
                let _ = child.kill();
                let _ = child.wait();
                break None;
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(5)),
            Err(_) => break None,
        }
    };
    let out = reader.join().unwrap_or_default();
    match status {
        Some(st) if st.success() => Ok(out),
        Some(_) => Err(GitError::Failed),
        None => Err(GitError::Timeout),
    }
}

fn git_text(root: &Path, args: &[&str], deadline: Instant) -> Result<String, GitError> {
    git(root, args, None, deadline).map(|b| String::from_utf8_lossy(&b).trim().to_string())
}

/// `None` when git says no (exit non-zero): an unset config, an unborn HEAD, no such ref.
fn git_opt(root: &Path, args: &[&str], deadline: Instant) -> Result<Option<String>, GitError> {
    match git_text(root, args, deadline) {
        Ok(s) if s.is_empty() => Ok(None),
        Ok(s) => Ok(Some(s)),
        Err(GitError::Failed) => Ok(None),
        Err(e) => Err(e),
    }
}

#[derive(Debug, Clone)]
struct Snap {
    root: PathBuf,
    common_dir: String,
    linked: bool,
    branch: Option<String>,
    head: Option<String>,
    default_ref: Option<String>,
    on_default: bool,
    base: String,
    tree: String,
    empty_tree: String,
}

fn snapshot(root: &Path, deadline: Instant) -> Result<Snap, &'static str> {
    if !root.exists() {
        return Err("gone");
    }
    let paths = git_text(
        root,
        &[
            "rev-parse",
            "--path-format=absolute",
            "--git-dir",
            "--git-common-dir",
            "--git-path",
            "index",
        ],
        deadline,
    )
    .map_err(|e| match e {
        GitError::Failed => "not_git",
        GitError::Timeout => "timeout",
    })?;
    let mut lines = paths.lines();
    let (Some(git_dir), Some(common_dir), Some(index)) = (lines.next(), lines.next(), lines.next())
    else {
        return Err("not_git");
    };
    let (git_dir, common_dir, index) = (
        git_dir.to_string(),
        common_dir.to_string(),
        PathBuf::from(index),
    );
    let d = |e: GitError| e.detail();
    if !git(root, &["ls-files", "--unmerged", "-z"], None, deadline)
        .map_err(d)?
        .is_empty()
    {
        return Err("unsupported");
    }
    for key in ["core.sparseCheckout", "index.sparse"] {
        if git_opt(root, &["config", "--type=bool", "--get", key], deadline)
            .map_err(d)?
            .as_deref()
            == Some("true")
        {
            return Err("unsupported");
        }
    }
    let empty_tree =
        git_text(root, &["hash-object", "-t", "tree", "/dev/null"], deadline).map_err(d)?;
    let mut attempt = 0;
    let (head, branch, tree) = loop {
        let head = git_opt(
            root,
            &["rev-parse", "--verify", "--quiet", "HEAD^{commit}"],
            deadline,
        )
        .map_err(d)?;
        let branch = git_opt(
            root,
            &["symbolic-ref", "--quiet", "--short", "HEAD"],
            deadline,
        )
        .map_err(d)?;
        let tree = write_tree(root, &index, deadline).map_err(d)?;
        let head2 = git_opt(
            root,
            &["rev-parse", "--verify", "--quiet", "HEAD^{commit}"],
            deadline,
        )
        .map_err(d)?;
        let branch2 = git_opt(
            root,
            &["symbolic-ref", "--quiet", "--short", "HEAD"],
            deadline,
        )
        .map_err(d)?;
        if head == head2 && branch == branch2 {
            break (head, branch, tree);
        }
        attempt += 1;
        if attempt == 2 {
            return Err("busy");
        }
    };
    let default_ref = default_branch(root, deadline).map_err(d)?;
    let default_short = default_ref
        .as_deref()
        .map(|r| r.strip_prefix("origin/").unwrap_or(r).to_string());
    let on_default = branch.is_some() && branch == default_short;
    let base = match (&head, &default_ref) {
        (None, _) => empty_tree.clone(),
        (Some(h), Some(def)) if !on_default => git_opt(root, &["merge-base", h, def], deadline)
            .map_err(d)?
            .unwrap_or_else(|| h.clone()),
        (Some(h), _) => h.clone(),
    };
    Ok(Snap {
        root: root.to_path_buf(),
        linked: git_dir != common_dir,
        common_dir,
        branch,
        head,
        default_ref,
        on_default,
        base,
        tree,
        empty_tree,
    })
}

/// `origin/HEAD`'s target, else a local `main`, else `master`.
fn default_branch(root: &Path, deadline: Instant) -> Result<Option<String>, GitError> {
    if let Some(r) = git_opt(
        root,
        &[
            "symbolic-ref",
            "--quiet",
            "--short",
            "refs/remotes/origin/HEAD",
        ],
        deadline,
    )? {
        return Ok(Some(r));
    }
    for b in ["main", "master"] {
        let r = format!("refs/heads/{b}^{{commit}}");
        if git_opt(root, &["rev-parse", "--verify", "--quiet", &r], deadline)?.is_some() {
            return Ok(Some(b.to_string()));
        }
    }
    Ok(None)
}

static TMP_SEQ: AtomicU64 = AtomicU64::new(0);

/// The working tree as a tree id: the index copied into a private temp dir, `add -A` into the
/// copy, `write-tree`, the dir removed. The real index is never touched, nor a lock on it. A
/// split index's shared file is read from the git dir, and the copy is written whole
/// (`core.splitIndex=false`), so no new shared file lands there.
fn write_tree(root: &Path, index: &Path, deadline: Instant) -> Result<String, GitError> {
    let nanos = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or_default();
    let tmp = std::env::temp_dir().join(format!(
        "fleet-diff-{}-{nanos:x}-{}",
        std::process::id(),
        TMP_SEQ.fetch_add(1, Ordering::Relaxed)
    ));
    fs::create_dir(&tmp).map_err(|_| GitError::Failed)?;
    let result = (|| {
        let copy = tmp.join("index");
        match fs::copy(index, &copy) {
            Ok(_) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(_) => return Err(GitError::Failed),
        }
        git(root, &["add", "-A", "--", "."], Some(&copy), deadline)?;
        let out = git(root, &["write-tree"], Some(&copy), deadline)?;
        Ok(String::from_utf8_lossy(&out).trim().to_string())
    })();
    let _ = fs::remove_dir_all(&tmp);
    result
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct FileRow {
    path: String,
    old_path: Option<String>,
    kind: &'static str,
    additions: u64,
    deletions: u64,
    binary: bool,
    uncommitted: bool,
}

struct Files {
    rows: Vec<FileRow>,
    additions: u64,
    deletions: u64,
}

fn files(s: &Snap, deadline: Instant) -> Result<Files, &'static str> {
    let d = |e: GitError| e.detail();
    let common = [
        "diff",
        "-z",
        "-M",
        "--no-ext-diff",
        "--no-textconv",
        "--no-color",
    ];
    let with = |extra: &[&str], from: &str| {
        let mut a: Vec<&str> = common.to_vec();
        a.extend_from_slice(extra);
        a.push(from);
        a.push(&s.tree);
        git(&s.root, &a, None, deadline)
    };
    let raw = with(&["--raw", "--no-abbrev"], &s.base).map_err(d)?;
    let numstat = with(&["--numstat"], &s.base).map_err(d)?;
    let head_side = s.head.as_deref().unwrap_or(&s.empty_tree);
    let uncommitted_raw = git(
        &s.root,
        &[
            "diff",
            "-z",
            "--no-renames",
            "--no-ext-diff",
            "--no-textconv",
            "--name-only",
            head_side,
            &s.tree,
        ],
        None,
        deadline,
    )
    .map_err(d)?;
    let uncommitted: HashSet<String> = split_z(&uncommitted_raw).into_iter().collect();
    let counts = parse_numstat(&numstat);
    let mut rows = parse_raw(&raw);
    let (mut additions, mut deletions) = (0, 0);
    for r in &mut rows {
        if let Some(&(a, dl, bin)) = counts.get(&r.path) {
            r.binary = bin;
            if !bin && !matches!(r.kind, "submodule" | "mode_changed") {
                r.additions = a;
                r.deletions = dl;
            }
        }
        r.uncommitted = uncommitted.contains(&r.path)
            || r.old_path.as_ref().is_some_and(|o| uncommitted.contains(o));
        additions += r.additions;
        deletions += r.deletions;
    }
    rows.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(Files {
        rows,
        additions,
        deletions,
    })
}

fn split_z(b: &[u8]) -> Vec<String> {
    b.split(|c| *c == 0)
        .filter(|s| !s.is_empty())
        .map(|s| String::from_utf8_lossy(s).into_owned())
        .collect()
}

/// `git diff --raw -z`: `:<old mode> <new mode> <old sha> <new sha> <status>\0<path>\0`, two
/// paths (old, new) for a rename or copy.
fn parse_raw(b: &[u8]) -> Vec<FileRow> {
    let toks = split_z(b);
    let mut out = Vec::new();
    let mut i = 0;
    while i < toks.len() {
        let meta = &toks[i];
        if !meta.starts_with(':') {
            i += 1;
            continue;
        }
        let f: Vec<&str> = meta[1..].split_whitespace().collect();
        if f.len() < 5 {
            i += 1;
            continue;
        }
        let (old_mode, new_mode, old_sha, new_sha, status) = (f[0], f[1], f[2], f[3], f[4]);
        let two = status.starts_with('R') || status.starts_with('C');
        let (old_path, path) = if two {
            (toks.get(i + 1).cloned(), toks.get(i + 2).cloned())
        } else {
            (None, toks.get(i + 1).cloned())
        };
        i += if two { 3 } else { 2 };
        let Some(path) = path else { break };
        let kind = if old_mode == "160000" || new_mode == "160000" {
            "submodule"
        } else {
            match status.chars().next() {
                Some('A') | Some('C') => "added",
                Some('D') => "deleted",
                Some('R') => "renamed",
                Some('T') => "type_changed",
                _ if old_sha == new_sha && old_mode != new_mode => "mode_changed",
                _ => "modified",
            }
        };
        out.push(FileRow {
            path,
            old_path: if kind == "renamed" { old_path } else { None },
            kind,
            additions: 0,
            deletions: 0,
            binary: false,
            uncommitted: false,
        });
    }
    out
}

/// `git diff --numstat -z`: `<added>\t<deleted>\t<path>\0`, or `<added>\t<deleted>\t\0<old>\0<new>\0`
/// for a rename; `-` counts for a binary file. Keyed by the (new) path.
fn parse_numstat(b: &[u8]) -> HashMap<String, (u64, u64, bool)> {
    let toks: Vec<String> = b
        .split(|c| *c == 0)
        .map(|s| String::from_utf8_lossy(s).into_owned())
        .collect();
    let mut out = HashMap::new();
    let mut i = 0;
    while i < toks.len() {
        let mut parts = toks[i].splitn(3, '\t');
        let (Some(a), Some(dl), Some(path)) = (parts.next(), parts.next(), parts.next()) else {
            i += 1;
            continue;
        };
        let (path, step) = if path.is_empty() {
            (toks.get(i + 2).cloned().unwrap_or_default(), 3)
        } else {
            (path.to_string(), 1)
        };
        i += step;
        let binary = a == "-" && dl == "-";
        out.insert(
            path,
            (a.parse().unwrap_or(0), dl.parse().unwrap_or(0), binary),
        );
    }
    out
}

fn candidate_json(c: &Candidate, s: &Snap, f: Files) -> Value {
    let total = f.rows.len();
    let listed: Vec<Value> = f
        .rows
        .iter()
        .take(MAX_FILES)
        .map(|r| {
            json!({
                "path": r.path,
                "kind": r.kind,
                "old_path": r.old_path,
                "additions": r.additions,
                "deletions": r.deletions,
                "binary": r.binary,
                "uncommitted": r.uncommitted,
            })
        })
        .collect();
    json!({
        "root": c.root.display().to_string(),
        "why": c.why,
        "repo": { "common_dir": s.common_dir, "linked_worktree": s.linked },
        "branch": s.branch,
        "head": s.head,
        "on_default_branch": s.on_default,
        "base": { "ref": s.default_ref, "sha": s.base },
        "stats": { "files": total, "additions": f.additions, "deletions": f.deletions },
        "files": listed,
        "more_files": total.saturating_sub(MAX_FILES),
    })
}

/// Opaque, 32 hex digits: two FNV-1a 64 hashes over one line per candidate. Equal fingerprints
/// mean equal answers.
fn fingerprint(rows: &[(Candidate, Result<Snap, &'static str>)]) -> String {
    let text: String = rows
        .iter()
        .map(|(c, s)| {
            let root = format!("{}[{}]", c.root.display(), c.why.join(","));
            match s {
                Ok(s) => format!(
                    "{root}={}:{}:{}:{}:{}\n",
                    s.base,
                    s.head.as_deref().unwrap_or("-"),
                    s.branch.as_deref().unwrap_or("-"),
                    s.default_ref.as_deref().unwrap_or("-"),
                    s.tree
                ),
                Err(d) => format!("{root}=skipped:{d}\n"),
            }
        })
        .collect();
    let fnv = |seed: u64| {
        text.bytes().fold(seed, |h, b| {
            (h ^ u64::from(b)).wrapping_mul(0x0100_0000_01b3)
        })
    };
    format!(
        "{:016x}{:016x}",
        fnv(0xcbf2_9ce4_8422_2325),
        fnv(0x6c62_272e_07bb_0142)
    )
}

/// `f` over `items`, [`WORKERS`] at a time, results in order.
fn in_parallel<T: Sync, R: Send>(items: &[T], f: impl Fn(&T) -> R + Sync) -> Vec<R> {
    let mut out = Vec::with_capacity(items.len());
    for chunk in items.chunks(WORKERS) {
        let results: Vec<R> = std::thread::scope(|s| {
            let handles: Vec<_> = chunk.iter().map(|it| s.spawn(|| f(it))).collect();
            handles
                .into_iter()
                .map(|h| h.join().expect("diff worker"))
                .collect()
        });
        out.extend(results);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn home() -> PathBuf {
        PathBuf::from("/Users/x")
    }

    #[test]
    fn c3_shell_writes_redirects_tee_and_cd() {
        let h = home();
        let w = |c: &str| shell_writes(c, &h);
        assert_eq!(w("echo hi > /tmp/a.txt"), [PathBuf::from("/tmp/a.txt")]);
        assert_eq!(
            w("echo hi >> ~/notes.md"),
            [PathBuf::from("/Users/x/notes.md")]
        );
        assert_eq!(w("cmd 2> /tmp/err.log"), [PathBuf::from("/tmp/err.log")]);
        assert_eq!(
            w("cd /repo/app && cat > src/x.ts <<'EOF'\nx\nEOF"),
            [PathBuf::from("/repo/app/src/x.ts")]
        );
        assert_eq!(
            w("ls | tee -a /tmp/out.txt"),
            [PathBuf::from("/tmp/out.txt")]
        );
        assert_eq!(
            w("echo > \"/tmp/with space.md\""),
            [PathBuf::from("/tmp/with space.md")]
        );
        assert!(w("cmd > /dev/null 2>&1").is_empty());
        assert!(w("cmd &> /tmp/both.log").is_empty());
        assert!(w("echo > $OUT").is_empty());
        assert!(
            w("echo > rel.txt").is_empty(),
            "relative without a cd: unknown"
        );
    }

    #[test]
    fn c3_edits_in_line_reads_edit_tools_and_bash_only() {
        let h = home();
        let line = json!({ "timestamp": "2026-10-31T10:00:00Z", "message": { "content": [
            { "type": "tool_use", "name": "Edit", "input": { "file_path": "/r/a.rs" } },
            { "type": "tool_use", "name": "MultiEdit", "input": { "file_path": "/r/b.rs", "edits": [{ "file_path": "/r/c.rs" }] } },
            { "type": "tool_use", "name": "NotebookEdit", "input": { "notebook_path": "/r/n.ipynb" } },
            { "type": "tool_use", "name": "Read", "input": { "file_path": "/elsewhere/read.rs" } },
            { "type": "tool_use", "name": "Bash", "input": { "command": "echo x > /r/d.txt" } },
            { "type": "tool_use", "name": "Write", "input": { "file_path": "relative.md" } }
        ]}})
        .to_string();
        let got: Vec<PathBuf> = edits_in_line(&line, &h)
            .into_iter()
            .map(|(p, _)| p)
            .collect();
        assert_eq!(
            got,
            ["/r/a.rs", "/r/b.rs", "/r/c.rs", "/r/n.ipynb", "/r/d.txt"].map(PathBuf::from)
        );
    }

    #[test]
    fn c2_parse_raw_kinds() {
        let raw = b":100644 100644 aaa bbb M\0src/a.rs\0:000000 100644 000 ccc A\0new.rs\0:100644 000000 ddd 000 D\0gone.rs\0:100644 100644 eee fff R087\0old.rs\0moved.rs\0:100644 100755 abc abc M\0run.sh\0:100644 120000 abc def T\0link\0:160000 160000 s1 s2 M\0sub\0";
        let rows = parse_raw(raw);
        let kinds: Vec<(&str, &str, Option<&str>)> = rows
            .iter()
            .map(|r| (r.path.as_str(), r.kind, r.old_path.as_deref()))
            .collect();
        assert_eq!(
            kinds,
            [
                ("src/a.rs", "modified", None),
                ("new.rs", "added", None),
                ("gone.rs", "deleted", None),
                ("moved.rs", "renamed", Some("old.rs")),
                ("run.sh", "mode_changed", None),
                ("link", "type_changed", None),
                ("sub", "submodule", None),
            ]
        );
    }

    #[test]
    fn c2_parse_numstat_renames_and_binaries() {
        let n = parse_numstat(b"3\t1\tsrc/a.rs\0-\t-\tlogo.png\0\x32\t0\t\0old.rs\0moved.rs\0");
        assert_eq!(n["src/a.rs"], (3, 1, false));
        assert_eq!(n["logo.png"], (0, 0, true));
        assert_eq!(n["moved.rs"], (2, 0, false));
    }

    #[test]
    fn c3_candidates_cwd_first_merged_and_capped() {
        let r = |s: &str| PathBuf::from(s);
        let edited: Vec<PathBuf> = (0..20)
            .map(|i| r(&format!("/e{i}")))
            .chain([r("/cwd")])
            .collect();
        let c = candidates(Some(r("/cwd")), edited);
        assert_eq!(c.len(), MAX_CANDIDATES);
        assert_eq!(
            c[0],
            Candidate {
                root: r("/cwd"),
                why: vec!["cwd", "edited"]
            }
        );
        assert_eq!(c[1].why, ["edited"]);
    }

    #[test]
    fn c3_checkout_never_home_or_root() {
        let t = tempfile::tempdir().unwrap();
        let home = t.path().join("home");
        fs::create_dir_all(home.join(".git")).unwrap();
        fs::create_dir_all(home.join("p/repo/.git")).unwrap();
        fs::create_dir_all(home.join("p/repo/src")).unwrap();
        let root = checkout_from(&home.join("p/repo/src/deep/missing"), &home).unwrap();
        assert!(root.ends_with("p/repo"));
        assert_eq!(
            checkout_from(&home.join("p/loose"), &home),
            None,
            "dotfiles repo at $HOME never counts"
        );
        // Symlinks resolve first, as git does: one into a repo finds it, one into $HOME doesn't.
        let into_repo = t.path().join("link-src");
        std::os::unix::fs::symlink(home.join("p/repo/src"), &into_repo).unwrap();
        assert_eq!(
            checkout_from(&into_repo.join("x.rs"), &home),
            Some(fs::canonicalize(home.join("p/repo")).unwrap())
        );
        fs::create_dir_all(home.join("notes")).unwrap();
        let into_home = t.path().join("link-notes");
        std::os::unix::fs::symlink(home.join("notes"), &into_home).unwrap();
        assert_eq!(checkout_from(&into_home.join("n.md"), &home), None);
    }

    #[test]
    fn c3_prune_keeps_a_state_whose_lock_is_held() {
        let t = tempfile::tempdir().unwrap();
        let old = SystemTime::now() - STATE_TTL - Duration::from_secs(60);
        for id in ["held", "free"] {
            let p = t.path().join(format!("{id}.json"));
            fs::write(&p, "{}").unwrap();
            File::options()
                .write(true)
                .open(&p)
                .unwrap()
                .set_modified(old)
                .unwrap();
        }
        let held = lock(&t.path().join("held.lock")).unwrap();
        prune(t.path());
        assert!(
            t.path().join("held.json").exists(),
            "its session still holds the lock"
        );
        assert!(!t.path().join("free.json").exists());
        drop(held);
        // A child another test thread forks can hold the lock's fd for a moment: a blocked prune
        // only waits for the next call.
        for _ in 0..40 {
            prune(t.path());
            if !t.path().join("held.json").exists() {
                break;
            }
            std::thread::sleep(Duration::from_millis(25));
        }
        assert!(!t.path().join("held.json").exists());
    }
}
