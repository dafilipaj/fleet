//! Claude Code subagent sidecars. Codex and pi counts are not supported yet.

use std::collections::HashMap;
use std::fs::{self, File};
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom};
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Counts {
    pub running: u64,
    pub finished: Option<u64>,
}

pub(crate) struct Scan {
    sidecars: Vec<Sidecar>,
}

/// A current-process sidecar. `finished: true` marks the end of a run, which may not be
/// the end of the subagent; a sidecar without it may have ended all the same, stopped
/// before its run could finish ([`Lifecycle`]).
struct Sidecar {
    id: String,
    finished: bool,
    /// `parentAgentId`: the owner whose own transcript receives its notifications.
    owner: Option<String>,
    log: PathBuf,
    /// The meta's mtime: the end of the run when `finished`. Otherwise it decides nothing
    /// about a stop, since worktree cleanup and a user stop rewrite the meta afterwards.
    mtime: SystemTime,
    /// `revivedAt`: when `SendMessage` last started a new run of an ended subagent.
    revived: Option<SystemTime>,
    /// `stoppedByUser`: Claude Code never revives such a subagent, so it has ended.
    stopped_by_user: bool,
}

#[derive(Deserialize)]
struct Meta {
    finished: Option<bool>,
    #[serde(rename = "parentAgentId")]
    parent_agent_id: Option<serde_json::Value>,
    /// Epoch milliseconds.
    #[serde(rename = "revivedAt")]
    revived_at: Option<serde_json::Value>,
    #[serde(rename = "stoppedByUser")]
    stopped_by_user: Option<serde_json::Value>,
}

const MAX_META_BYTES: u64 = 1024 * 1024;
/// How much of a nested subagent's owner transcript is searched for its notifications.
const OWNER_TAIL: u64 = 256 * 1024;
/// Generated `<note>` text of a `completed` notification whose subagent still waits on
/// background work of its own and will run again under the same id.
const INTERIM_NOTE: &str = "background work of its own still running";
/// Appended to a notification's note when new input resumes the subagent at once.
const PENDING_INPUT_NOTE: &str = "arrived after this run";
/// Notification statuses that end a subagent unless their note says it runs on.
const TERMINAL: [&str; 4] = ["completed", "failed", "killed", "stopped"];

/// Undocumented sidecars gained `finished` in 2.1.301. Without the registry's start
/// time we cannot distinguish a running task from a crash left by an older process.
pub(crate) fn scan(
    transcript: &Path,
    version: Option<&str>,
    started_at: Option<i64>,
) -> Option<Scan> {
    let parts: Vec<u64> = version?
        .split('.')
        .map(str::parse)
        .collect::<Result<_, _>>()
        .ok()?;
    if parts.len() != 3 || (parts[0], parts[1], parts[2]) < (2, 1, 301) || !transcript.is_file() {
        return None;
    }
    let started = UNIX_EPOCH.checked_add(Duration::from_millis(started_at?.try_into().ok()?))?;
    let dir = transcript.with_extension("").join("subagents");
    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Some(Scan {
                sidecars: Vec::new(),
            });
        }
        Err(_) => return None,
    };
    let mut scan = Scan {
        sidecars: Vec::new(),
    };
    for entry in entries {
        let entry = entry.ok()?;
        let path = entry.path();
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        let Some(id) = name
            .strip_prefix("agent-")
            .and_then(|n| n.strip_suffix(".meta.json"))
        else {
            continue;
        };
        let Ok(stat) = entry.metadata() else { continue };
        let Ok(mtime) = stat.modified() else { continue };
        if !stat.is_file() || mtime < started || stat.len() > MAX_META_BYTES {
            continue;
        }
        let Some(meta) = read_meta(&path) else {
            continue;
        };
        scan.sidecars.push(Sidecar {
            id: id.to_owned(),
            finished: meta.finished == Some(true),
            owner: meta
                .parent_agent_id
                .as_ref()
                .and_then(serde_json::Value::as_str)
                .filter(|owner| is_agent_id(owner))
                .map(str::to_owned),
            log: path.with_file_name(format!("agent-{id}.jsonl")),
            mtime,
            revived: meta
                .revived_at
                .as_ref()
                .and_then(serde_json::Value::as_u64)
                .map(|ms| UNIX_EPOCH + Duration::from_millis(ms)),
            stopped_by_user: meta.stopped_by_user == Some(serde_json::Value::Bool(true)),
        });
    }
    Some(scan)
}

/// An id that names a file in the same directory, never a path out of it.
fn is_agent_id(id: &str) -> bool {
    !id.is_empty()
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

/// What a Session's current sidecars ask of the parent Transcript's tail: its Turn's
/// prompt and the subagents' [`Lifecycle`] evidence ([`Scan::search`]).
#[derive(Debug, Default, Clone)]
pub(crate) struct Search {
    /// `(agent id, meta mtime)` of the finished depth-1 sidecars: the tail widens while one
    /// has no evidence in it and ended before the tail's oldest entry. A nested one's
    /// notifications go to its owner, so they never widen the parent's read.
    pub(crate) awaited: Vec<(String, SystemTime)>,
    /// `(agent id, log mtime)` of the unfinished depth-1 sidecars whose `agent-<id>.jsonl`
    /// is a regular file: the tail widens while one has no evidence in it and its log was
    /// last written before the tail's oldest entry. One quiet since then may have been
    /// stopped further back; one written to since ended, if at all, inside the tail.
    pub(crate) quiet: Vec<(String, SystemTime)>,
    /// Widen until the Turn's prompt is in the tail, as a finished sidecar needs. Unfinished
    /// ones don't need it: they count as running without it.
    pub(crate) turn: bool,
}

impl Scan {
    /// Current sidecars need the Turn's start and the Transcript's [`Lifecycle`] evidence;
    /// without any, neither is searched.
    pub(crate) fn search(&self) -> Option<Search> {
        if self.sidecars.is_empty() {
            return None;
        }
        let finished = || self.sidecars.iter().filter(|s| s.finished);
        Some(Search {
            awaited: finished()
                .filter(|s| s.owner.is_none())
                .map(|s| (s.id.clone(), s.mtime))
                .collect(),
            // One stat per running depth-1 sidecar's log; the log itself is never opened.
            quiet: self
                .sidecars
                .iter()
                .filter(|s| !s.finished && s.owner.is_none())
                .filter_map(|s| {
                    let log = fs::metadata(&s.log).ok().filter(|m| m.is_file())?;
                    Some((s.id.clone(), log.modified().ok()?))
                })
                .collect(),
            turn: finished().next().is_some(),
        })
    }

    /// `lifecycle` is the parent Transcript's evidence from the same tail as `turn_start`.
    /// A nested subagent's owner transcript is searched too, once per owner per call.
    pub(crate) fn counts(self, turn_start: Option<&str>, lifecycle: &Lifecycle) -> Counts {
        let mut owners: HashMap<String, Lifecycle> = HashMap::new();
        let mut running = 0;
        // `(end, log)` of each sidecar that ended.
        let mut settled = Vec::new();
        for s in self.sidecars {
            let owner = s.owner.as_ref().map(|owner| {
                &*owners.entry(owner.clone()).or_insert_with(|| {
                    owner_tail(&s.log.with_file_name(format!("agent-{owner}.jsonl")))
                })
            });
            // A revive (`SendMessage`) clears `finished` and records `revivedAt`: evidence
            // older than the revive, or undated, belongs to an earlier run and decides
            // nothing about this one.
            let latest = Lifecycle::latest(lifecycle, owner, &s.id).filter(|l| {
                s.finished
                    || s.revived
                        .is_none_or(|revived| l.at.is_some_and(|at| at >= revived))
            });
            // Claude Code never revives a subagent the user stopped.
            if !s.stopped_by_user && latest.and_then(|l| l.running).unwrap_or(!s.finished) {
                running += 1;
                continue;
            }
            // A finished run ended at its meta's last write. An unfinished one ended with
            // the evidence that settled it.
            let end = match latest.and_then(|l| l.at) {
                Some(at) if !s.finished => at,
                _ => s.mtime,
            };
            settled.push((end, s.log));
        }
        let finished = if settled.is_empty() {
            Some(0)
        } else {
            turn_start.and_then(timestamp).map(|turn| {
                settled
                    .iter()
                    .filter(|(end, log)| {
                        *end >= turn && first_timestamp(log).is_some_and(|start| start >= turn)
                    })
                    .count() as u64
            })
        };
        Counts { running, finished }
    }
}

/// The latest lifecycle evidence per subagent id in one transcript tail: its task
/// notifications and successful `TaskStop` results. Built by a reverse pass, so the first
/// decisive entry seen for an id is its latest.
#[derive(Debug, Default, Clone)]
pub(crate) struct Lifecycle(HashMap<String, Latest>);

#[derive(Debug, Clone, Copy)]
struct Latest {
    at: Option<SystemTime>,
    /// `Some(true)`: the run ended but the subagent did not (an interim or pending-input
    /// note). `Some(false)`: it ended (a terminal status or a successful `TaskStop`).
    /// `None`: a status Fleet doesn't know, which leaves it to the sidecar's `finished`.
    running: Option<bool>,
}

impl Lifecycle {
    /// Cheap prefilter for a reverse pass: only these lines are parsed for evidence.
    pub(crate) fn candidate(line: &str) -> bool {
        // Unquoted: a `TaskStop` result may carry it only inside an escaped JSON string.
        line.contains("task-notification") || line.contains("local_agent")
    }

    /// Record `entry` unless a later one (seen earlier in the reverse pass) decided its id.
    /// Within the entry, a later notification for an id beats an earlier one.
    pub(crate) fn observe(&mut self, entry: &serde_json::Value) {
        #[cfg(test)]
        tests::OBSERVED.with(|n| n.set(n.get() + 1));
        let at = entry["timestamp"].as_str().and_then(timestamp);
        for (id, running) in evidence(entry).into_iter().rev() {
            self.0.entry(id).or_insert(Latest { at, running });
        }
    }

    /// Whether this tail holds decisive evidence for `id`.
    pub(crate) fn decides(&self, id: &str) -> bool {
        self.0.contains_key(id)
    }

    /// The later of the parent's and the owner's evidence; on a tie, the owner's.
    fn latest(parent: &Self, owner: Option<&Self>, id: &str) -> Option<Latest> {
        let parent = parent.0.get(id).copied();
        let owner = owner.and_then(|o| o.0.get(id).copied());
        match (parent, owner) {
            (Some(p), Some(o)) => Some(if p.at > o.at { p } else { o }),
            (p, o) => o.or(p),
        }
    }
}

/// `(agent id, still running)` ([`Latest::running`]), in entry order, for each task
/// notification with a `<status>` in `v`, or for a successful `TaskStop` of a subagent.
/// Anything else, routing notices included, decides nothing.
fn evidence(v: &serde_json::Value) -> Vec<(String, Option<bool>)> {
    let attachment = &v["attachment"];
    let content = match v["type"].as_str() {
        Some("user") if v["origin"]["kind"] == "task-notification" => &v["message"]["content"],
        Some("attachment")
            if attachment["type"] == "queued_command"
                && attachment["commandMode"] == "task-notification" =>
        {
            &attachment["prompt"]
        }
        Some("user") => return stopped(v).map(|id| (id, Some(false))).into_iter().collect(),
        _ => return Vec::new(),
    };
    let texts: Vec<&str> = match content {
        serde_json::Value::String(text) => vec![text],
        serde_json::Value::Array(blocks) => blocks
            .iter()
            .filter(|b| b["type"] == "text")
            .filter_map(|b| b["text"].as_str())
            .collect(),
        _ => return Vec::new(),
    };
    texts
        .into_iter()
        .flat_map(notifications)
        .filter_map(|n| {
            let status = n.status?;
            let note = n.note.unwrap_or_default();
            let running = if note.contains(PENDING_INPUT_NOTE)
                || (status == "completed" && note.contains(INTERIM_NOTE))
            {
                Some(true)
            } else if TERMINAL.contains(&status) {
                Some(false)
            } else {
                None
            };
            Some((n.task_id?.to_owned(), running))
        })
        .collect()
}

/// A subagent's id from a `TaskStop` result that is not an error. `TaskStop` also stops
/// shells, so the result must name a `local_agent`. Read from the structured
/// `toolUseResult` when the entry has one, else from the `tool_result` content: a JSON
/// string, or text blocks holding one.
fn stopped(v: &serde_json::Value) -> Option<String> {
    let results: Vec<_> = v["message"]["content"]
        .as_array()?
        .iter()
        .filter(|b| b["type"] == "tool_result")
        .collect();
    if results.is_empty() || results.iter().any(|b| b["is_error"] == true) {
        return None;
    }
    let agent = |r: &serde_json::Value| {
        (r["task_type"] == "local_agent")
            .then(|| r["task_id"].as_str().map(str::to_owned))
            .flatten()
    };
    if v["toolUseResult"].is_object() {
        return agent(&v["toolUseResult"]);
    }
    results
        .iter()
        .flat_map(|b| match &b["content"] {
            serde_json::Value::String(text) => vec![text.as_str()],
            serde_json::Value::Array(blocks) => blocks
                .iter()
                .filter(|b| b["type"] == "text")
                .filter_map(|b| b["text"].as_str())
                .collect(),
            _ => Vec::new(),
        })
        .find_map(|text| agent(&serde_json::from_str(text).ok()?))
}

#[derive(Default)]
struct Notification<'a> {
    task_id: Option<&'a str>,
    status: Option<&'a str>,
    note: Option<&'a str>,
}

const OPEN: &str = "<task-notification>";
const CLOSE: &str = "</task-notification>";

/// Every top-level generated `<task-notification>` element in `text`: the one that opens
/// it, then each one joined after a closing tag and whitespace, as Claude Code joins the
/// notifications it delivers together in one prompt (`\n\n`, such as the queued ones of a
/// parked owner that resumes). Only each element's direct children are read.
fn notifications(text: &str) -> impl Iterator<Item = Notification<'_>> {
    let mut next = text.trim_start().strip_prefix(OPEN);
    std::iter::from_fn(move || {
        let (n, rest) = children(next?);
        next = joined(rest);
        Some(n)
    })
}

/// The body of the next element joined after a closing tag in `rest`, skipping a closing
/// tag that isn't followed by an opening one (one quoted inside a `<result>`, say).
fn joined(mut rest: &str) -> Option<&str> {
    while let Some(i) = rest.find(CLOSE) {
        rest = &rest[i + CLOSE.len()..];
        if let Some(body) = rest.trim_start().strip_prefix(OPEN) {
            return Some(body);
        }
    }
    None
}

/// The direct children at the start of one element's body, and the text where the walk
/// stopped. `<summary>` is skipped over, never searched, and the walk stops at `<result>`:
/// both can quote anything, notification-like markup included.
fn children(mut rest: &str) -> (Notification<'_>, &str) {
    let mut n = Notification::default();
    while let Some(open) = rest.trim_start().strip_prefix('<') {
        let Some(end) = open.find('>') else { break };
        let name = &open[..end];
        // `</task-notification>` (or anything that isn't an element name) ends the children.
        if name.is_empty()
            || name == "result"
            || !name.bytes().all(|b| b.is_ascii_lowercase() || b == b'-')
        {
            break;
        }
        let body = &open[end + 1..];
        let close = format!("</{name}>");
        let Some(stop) = body.find(&close) else { break };
        let value = body[..stop].trim();
        let slot = match name {
            "task-id" => Some(&mut n.task_id),
            "status" => Some(&mut n.status),
            "note" => Some(&mut n.note),
            _ => None,
        };
        if let Some(slot) = slot {
            slot.get_or_insert(value);
        }
        rest = &body[stop + close.len()..];
    }
    (n, rest)
}

/// The last [`OWNER_TAIL`] of a nested subagent's owner transcript, under the same
/// regular-file rules as every other sidecar read.
fn owner_tail(path: &Path) -> Lifecycle {
    let mut lifecycle = Lifecycle::default();
    if !fs::metadata(path).is_ok_and(|m| m.is_file()) {
        return lifecycle;
    }
    let Ok(mut file) = open(path) else {
        return lifecycle;
    };
    let Ok(size) = file.metadata().map(|m| m.len()) else {
        return lifecycle;
    };
    let start = size.saturating_sub(OWNER_TAIL);
    let mut buf = Vec::new();
    if file.seek(SeekFrom::Start(start)).is_err()
        || file.take(OWNER_TAIL).read_to_end(&mut buf).is_err()
    {
        return lifecycle;
    }
    let text = String::from_utf8_lossy(&buf);
    let text = if start > 0 {
        // The first line is almost certainly cut in half.
        &text[text.find('\n').map_or(text.len(), |i| i + 1)..]
    } else {
        &text
    };
    for line in text.lines().rev().filter(|l| Lifecycle::candidate(l)) {
        if let Ok(v) = serde_json::from_str(line) {
            lifecycle.observe(&v);
        }
    }
    lifecycle
}

/// Bound the read too: a regular sidecar may grow after its directory stat.
fn read_meta(path: &Path) -> Option<Meta> {
    let mut bytes = Vec::new();
    open(path)
        .ok()?
        .take(MAX_META_BYTES + 1)
        .read_to_end(&mut bytes)
        .ok()?;
    if bytes.len() as u64 > MAX_META_BYTES {
        return None;
    }
    serde_json::from_slice(&bytes).ok()
}

fn timestamp(text: &str) -> Option<SystemTime> {
    chrono::DateTime::parse_from_rfc3339(text)
        .ok()
        .map(SystemTime::from)
}

fn open(path: &Path) -> std::io::Result<File> {
    #[cfg(test)]
    {
        tests::replace_before_open(path);
        tests::OPENED.with(|opened| opened.borrow_mut().push(path.to_path_buf()));
    }
    // O_NONBLOCK ABI values: std exposes custom_flags but not these constants.
    // Keep them local rather than adding a crate; unknown platforms fail closed.
    let nonblock = match std::env::consts::OS {
        "macos" | "ios" | "tvos" | "watchos" | "visionos" | "freebsd" | "netbsd" | "openbsd"
        | "dragonfly" => 0x4,
        "linux" | "android" => match std::env::consts::ARCH {
            "mips" | "mips64" | "mips32r6" | "mips64r6" => 0x80,
            "sparc" | "sparc64" => 0x4000,
            _ => 0x800,
        },
        _ => return Err(std::io::ErrorKind::Unsupported.into()),
    };
    let file = fs::OpenOptions::new()
        .read(true)
        .custom_flags(nonblock)
        .open(path)?;
    // fstat the descriptor, not the path: it may have been replaced since the
    // earlier stat. Never read a FIFO/device even if its nonblocking open succeeds.
    if !file.metadata()?.is_file() {
        return Err(std::io::Error::other("subagent file is not regular"));
    }
    Ok(file)
}

/// One bounded line, only for candidates that ended in this Turn. A running task's
/// log is never opened: a long, quiet tool does not mean the task has stopped.
fn first_timestamp(path: &Path) -> Option<SystemTime> {
    if !fs::metadata(path).ok()?.is_file() {
        return None;
    }
    let mut line = Vec::new();
    BufReader::new(open(path).ok()?)
        .take(1024 * 1024)
        .read_until(b'\n', &mut line)
        .ok()?;
    let v: serde_json::Value = serde_json::from_slice(&line).ok()?;
    timestamp(v["timestamp"].as_str()?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::{self, File, FileTimes};
    use std::time::{Duration, UNIX_EPOCH};

    const START: i64 = 1_767_225_600_000;
    const TURN: &str = "2026-01-01T00:01:00Z";

    thread_local! {
        pub(super) static OPENED: std::cell::RefCell<Vec<PathBuf>> = const { std::cell::RefCell::new(Vec::new()) };
        /// Transcript entries parsed for lifecycle evidence, to prove when none are.
        pub(super) static OBSERVED: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
        static REPLACE_BEFORE_OPEN: std::cell::RefCell<Option<(PathBuf, PathBuf)>> = const { std::cell::RefCell::new(None) };
    }

    pub(super) fn replace_before_open(path: &Path) {
        REPLACE_BEFORE_OPEN.with(|replacement| {
            let mut replacement = replacement.borrow_mut();
            if replacement
                .as_ref()
                .is_some_and(|(target, _)| target == path)
            {
                let (target, fifo) = replacement.take().unwrap();
                fs::rename(fifo, target).unwrap();
            }
        });
    }

    fn run_fifo_child(case: &str) {
        // assert_cmd kills and reaps a timed-out child. No FIFO writer or worker
        // thread can keep the parent test waiting for EOF or a join.
        let (_dir, path) = fixture(); // The parent also cleans up after a killed child.
        assert_cmd::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "core::subagents::tests::fifo_poll_child",
                "--nocapture",
            ])
            .env("FLEET_SUBAGENT_FIFO_CASE", case)
            .env("FLEET_SUBAGENT_FIFO_PATH", path)
            .timeout(Duration::from_secs(5))
            .assert()
            .success();
    }

    fn fixture() -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../testdata/subagents");
        let agents = dir.path().join("session/subagents");
        fs::create_dir_all(&agents).unwrap();
        fs::copy(source.join("typed.jsonl"), dir.path().join("session.jsonl")).unwrap();
        for entry in fs::read_dir(source.join("session/subagents")).unwrap() {
            let path = entry.unwrap().path();
            let dest = agents.join(path.file_name().unwrap());
            fs::copy(&path, &dest).unwrap();
            let seconds = match path.file_name().unwrap().to_str().unwrap() {
                "agent-stale.meta.json" => -1,
                "agent-prior.meta.json" | "agent-running.meta.json" => 30,
                _ => 120,
            };
            let at = UNIX_EPOCH + Duration::from_secs((START / 1000 + seconds) as u64);
            File::options()
                .write(true)
                .open(dest)
                .unwrap()
                .set_times(FileTimes::new().set_modified(at))
                .unwrap();
        }
        let path = dir.path().join("session.jsonl");
        (dir, path)
    }

    #[test]
    fn sidecars_count_running_finished_stopped_nested_and_forks_once() {
        let (_dir, path) = fixture();
        let s = scan(&path, Some("2.1.301"), Some(START)).unwrap();
        assert!(s.search().unwrap().turn);
        assert_eq!(
            s.counts(Some(TURN), &Lifecycle::default()),
            Counts {
                running: 2,
                finished: Some(3)
            }
        );
        // Old-turn finishes and a task spanning turns do not count as done. A running
        // task's transcript is deliberately absent: waiting on a tool still counts.
        let s = scan(&path, Some("2.1.308"), Some(START)).unwrap();
        assert_eq!(
            s.counts(None, &Lifecycle::default()),
            Counts {
                running: 2,
                finished: None
            }
        );
    }

    #[test]
    fn versions_missing_data_and_resumed_sessions() {
        let (_dir, path) = fixture();
        for version in [None, Some("2.1.300"), Some("invalid")] {
            assert!(scan(&path, version, Some(START)).is_none());
        }
        assert!(scan(&path, Some("2.1.301"), None).is_none());
        assert!(
            scan(
                &path.with_file_name("missing.jsonl"),
                Some("2.1.301"),
                Some(START)
            )
            .is_none()
        );
        let s = scan(&path, Some("2.1.301"), Some(START + 121_000)).unwrap();
        assert!(s.search().is_none());
        assert_eq!(
            s.counts(None, &Lifecycle::default()),
            Counts {
                running: 0,
                finished: Some(0)
            }
        );
        fs::remove_dir_all(path.with_extension("").join("subagents")).unwrap();
        assert_eq!(
            scan(&path, Some("2.1.301"), Some(START))
                .unwrap()
                .counts(None, &Lifecycle::default()),
            Counts {
                running: 0,
                finished: Some(0)
            }
        );
    }

    #[test]
    fn each_poll_observes_rewrites_torn_files_and_disappearance() {
        let (_dir, path) = fixture();
        assert_eq!(
            scan(&path, Some("2.1.301"), Some(START))
                .unwrap()
                .counts(Some(TURN), &Lifecycle::default())
                .running,
            2
        );
        let agent = path
            .with_extension("")
            .join("subagents/agent-running.meta.json");
        fs::write(&agent, r#"{"finished":true}"#).unwrap();
        let log = agent.with_file_name("agent-running.jsonl");
        fs::write(&log, "{\"timestamp\":\"2026-01-01T00:01:10Z\"}\n").unwrap();
        assert_eq!(
            scan(&path, Some("2.1.301"), Some(START))
                .unwrap()
                .counts(Some(TURN), &Lifecycle::default()),
            Counts {
                running: 1,
                finished: Some(4)
            }
        );
        fs::write(&agent, "{").unwrap();
        assert_eq!(
            scan(&path, Some("2.1.301"), Some(START))
                .unwrap()
                .counts(Some(TURN), &Lifecycle::default())
                .finished,
            Some(3)
        );
        fs::remove_file(agent).unwrap();
        assert_eq!(
            scan(&path, Some("2.1.301"), Some(START))
                .unwrap()
                .counts(Some(TURN), &Lifecycle::default())
                .running,
            1
        );
    }

    #[test]
    fn equality_boundaries_and_unreadable_first_timestamps() {
        let (_dir, path) = fixture();
        let agents = path.with_extension("").join("subagents");
        let at = UNIX_EPOCH + Duration::from_millis(START as u64);
        File::options()
            .write(true)
            .open(agents.join("agent-stale.meta.json"))
            .unwrap()
            .set_times(FileTimes::new().set_modified(at))
            .unwrap();
        // Equality with the process start is live, even if it predates this Turn.
        assert_eq!(
            scan(&path, Some("2.1.301"), Some(START))
                .unwrap()
                .counts(Some(TURN), &Lifecycle::default())
                .running,
            3
        );
        fs::write(
            agents.join("agent-finished.jsonl"),
            format!("{{\"timestamp\":\"{TURN}\"}}\n"),
        )
        .unwrap();
        let turn = timestamp(TURN).unwrap();
        File::options()
            .write(true)
            .open(agents.join("agent-finished.meta.json"))
            .unwrap()
            .set_times(FileTimes::new().set_modified(turn))
            .unwrap();
        assert_eq!(
            scan(&path, Some("2.1.301"), Some(START))
                .unwrap()
                .counts(Some(TURN), &Lifecycle::default())
                .finished,
            Some(3)
        );
        // Never search later lines for a more convenient start time.
        fs::write(
            agents.join("agent-finished.jsonl"),
            format!("{{}}\n{{\"timestamp\":\"{TURN}\"}}\n"),
        )
        .unwrap();
        fs::remove_file(agents.join("agent-stopped.jsonl")).unwrap();
        assert_eq!(
            scan(&path, Some("2.1.301"), Some(START))
                .unwrap()
                .counts(Some(TURN), &Lifecycle::default())
                .finished,
            Some(1)
        );
    }

    /// The lifecycle fixture: every meta current, the finished ones written 120 s after the
    /// process start and the unfinished ones at their launch, 65 s after it, since a stop
    /// never rewrites a meta; the Turn starts at 00:01 and every log starts at 00:01:10.
    fn lifecycle_fixture() -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let source =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../../testdata/subagents/lifecycle");
        let agents = dir.path().join("session/subagents");
        fs::create_dir_all(&agents).unwrap();
        fs::copy(
            source.join("session.jsonl"),
            dir.path().join("session.jsonl"),
        )
        .unwrap();
        for entry in fs::read_dir(source.join("session/subagents")).unwrap() {
            let path = entry.unwrap().path();
            let name = path.file_name().unwrap().to_str().unwrap();
            let seconds = if name.starts_with("agent-unfinished-") {
                65
            } else {
                120
            };
            let dest = agents.join(name);
            fs::copy(&path, &dest).unwrap();
            set_mtime(&dest, seconds);
        }
        let path = dir.path().join("session.jsonl");
        (dir, path)
    }

    /// Set `path`'s mtime `seconds` after the process start.
    fn set_mtime(path: &Path, seconds: u64) {
        File::options()
            .write(true)
            .open(path)
            .unwrap()
            .set_times(
                FileTimes::new()
                    .set_modified(UNIX_EPOCH + Duration::from_secs(START as u64 / 1000 + seconds)),
            )
            .unwrap();
    }

    /// What `list` does per Claude Code Session: the sidecars decide whether the shared
    /// tail searches for the Turn and the lifecycle evidence.
    fn poll(path: &Path) -> Counts {
        let s = scan(path, Some("2.1.308"), Some(START)).unwrap();
        let search = s.search();
        let (_, turn, lifecycle) =
            crate::core::context::for_transcript(path, None, search.as_ref());
        s.counts(turn.as_deref(), &lifecycle)
    }

    /// The parent's evidence from the lifecycle fixture's whole Transcript.
    fn parent_lifecycle(path: &Path) -> Lifecycle {
        let search = Search {
            awaited: Vec::new(),
            quiet: Vec::new(),
            turn: true,
        };
        crate::core::context::for_transcript(path, None, Some(&search)).2
    }

    #[test]
    fn a_finished_run_counts_as_running_until_its_latest_notification_settles_it() {
        let (_dir, path) = lifecycle_fixture();
        let parent = parent_lifecycle(&path);
        let owner = owner_tail(&path.with_extension("").join("subagents/agent-owner.jsonl"));
        for (id, running) in [
            // Interim note, even one written before the Turn's prompt.
            ("interim", true),
            ("settled", false),
            // Interim, then the settled note.
            ("resettled", false),
            // Interim, then a successful TaskStop: its only end.
            ("stopped", false),
            // Interim, then a TaskStop that failed.
            ("stop-failed", true),
            // New input arrived after a failed run: it resumes at once.
            ("pending", true),
            // Interim and pending-input notes only inside <summary> and <result>.
            ("quoted", false),
            // Interim, then a routing notice without a status, which decides nothing.
            ("routed", true),
            // Owner's later interim note beats the parent's earlier settled one.
            ("nested", true),
            // Owner's later settled note beats the parent's earlier interim one.
            ("nested-settled", false),
            // One prompt joining queued notices: interim, then settled, for one id, and
            // another id's interim after them. Parent as a string, owner as a text block.
            ("joined", false),
            ("joined-late", true),
            ("nested-joined", false),
            ("nested-joined-late", true),
        ] {
            let nested = id.starts_with("nested").then_some(&owner);
            let latest = Lifecycle::latest(&parent, nested, id).unwrap();
            assert_eq!(latest.running, Some(running), "{id}");
        }
        // No notification at all: finished, as before.
        assert!(Lifecycle::latest(&parent, None, "silent").is_none());
        // Owner (running), interim, stop-failed, pending, routed, nested, joined-late,
        // nested-joined-late and three unfinished run; settled, resettled, stopped, quoted,
        // silent, nested-settled, joined, nested-joined and two unfinished finished in this
        // Turn.
        assert_eq!(
            poll(&path),
            Counts {
                running: 11,
                finished: Some(10)
            }
        );
    }

    #[test]
    fn an_unfinished_sidecar_counts_as_finished_once_its_latest_evidence_ends_it() {
        let (_dir, path) = lifecycle_fixture();
        let parent = parent_lifecycle(&path);
        for (id, running) in [
            // A successful TaskStop whose result is only a JSON string in its content.
            ("unfinished-stopped", Some(false)),
            // A killed notification (and the queue record removing it, which is ignored).
            ("unfinished-killed", Some(false)),
            // A TaskStop that failed decides nothing.
            ("unfinished-stop-failed", None),
            // Stopped, then a later interim note: it runs again.
            ("unfinished-revived", Some(true)),
            // No evidence: the sidecar's own state, running.
            ("unfinished-silent", None),
        ] {
            let latest = Lifecycle::latest(&parent, None, id);
            assert_eq!(latest.and_then(|l| l.running), running, "{id}");
        }
        // Only the five unfinished sidecars: the stopped and the killed one ended in this
        // Turn, the other three run.
        let agents = path.with_extension("").join("subagents");
        for entry in fs::read_dir(&agents).unwrap() {
            let file = entry.unwrap().path();
            let name = file.file_name().unwrap().to_str().unwrap();
            if !name.starts_with("agent-unfinished-") {
                fs::remove_file(file).unwrap();
            }
        }
        let s = scan(&path, Some("2.1.308"), Some(START)).unwrap();
        let search = s.search().unwrap();
        // Unfinished sidecars never widen the read, for the prompt or for their evidence.
        assert!(!search.turn && search.awaited.is_empty());
        let counts = Counts {
            running: 3,
            finished: Some(2),
        };
        assert_eq!(poll(&path), counts);
        // A settled unfinished sidecar ends with the evidence that settled it, not with its
        // meta's write: the same stop entry before the Turn's prompt counts it in no Turn.
        let early = |id: &str, at: &str| {
            let mut lifecycle = Lifecycle::default();
            lifecycle.observe(&serde_json::json!({"type": "user", "timestamp": at,
                "message": {"content": [{"type": "tool_result", "content": format!(
                    r#"{{"task_id":"{id}","task_type":"local_agent"}}"#)}]}}));
            lifecycle
        };
        set_mtime(&agents.join("agent-unfinished-silent.meta.json"), 30);
        for (at, finished) in [("2026-01-01T00:00:59Z", 0), (TURN, 1)] {
            let counts = scan(&path, Some("2.1.308"), Some(START))
                .unwrap()
                .counts(Some(TURN), &early("unfinished-silent", at));
            assert_eq!(
                counts,
                Counts {
                    running: 4,
                    finished: Some(finished)
                },
                "{at}"
            );
        }
        // Undated evidence settles a sidecar never revived, but can't be placed after a
        // revive, so then it settles nothing.
        let mut undated = Lifecycle::default();
        undated.observe(&serde_json::json!({"type": "user", "message": {"content": [
            {"type": "tool_result", "content":
                r#"{"task_id":"unfinished-silent","task_type":"local_agent"}"#}]}}));
        let running = || {
            scan(&path, Some("2.1.308"), Some(START))
                .unwrap()
                .counts(Some(TURN), &undated)
                .running
        };
        assert_eq!(running(), 4);
        let meta = agents.join("agent-unfinished-silent.meta.json");
        fs::write(&meta, r#"{"spawnDepth":1,"revivedAt":1767225630000}"#).unwrap();
        set_mtime(&meta, 30);
        assert_eq!(running(), 5);
    }

    #[test]
    fn only_a_revive_after_its_stop_runs_a_stopped_sidecar_again() {
        let (_dir, path) = lifecycle_fixture();
        let agents = path.with_extension("").join("subagents");
        // Rewrite the meta with `extra` fields, later than any evidence.
        let rewrite = |id: &str, extra: serde_json::Value| {
            let file = agents.join(format!("agent-{id}.meta.json"));
            let mut meta: serde_json::Value =
                serde_json::from_slice(&fs::read(&file).unwrap()).unwrap();
            meta.as_object_mut()
                .unwrap()
                .extend(extra.as_object().unwrap().clone());
            fs::write(&file, meta.to_string()).unwrap();
            set_mtime(&file, 200);
        };
        let at = |seconds: u64| (START as u64 / 1000 + seconds) * 1000;
        let base = poll(&path);
        // The killed notice is at 00:01:44 and the TaskStop at 00:01:43.
        for (id, stop) in [("unfinished-killed", 104), ("unfinished-stopped", 103)] {
            // Worktree cleanup and a user stop rewrite the meta after the stop.
            rewrite(id, serde_json::json!({"worktreeCleanlyRemoved": true}));
            assert_eq!(poll(&path), base, "{id} rewritten after its stop");
            rewrite(id, serde_json::json!({"revivedAt": at(stop - 1)}));
            assert_eq!(poll(&path), base, "{id} revived before its stop");
            rewrite(id, serde_json::json!({"revivedAt": at(stop + 1)}));
            assert_eq!(
                poll(&path),
                Counts {
                    running: base.running + 1,
                    finished: base.finished.map(|n| n - 1)
                },
                "{id} revived after its stop"
            );
            rewrite(id, serde_json::json!({"revivedAt": null}));
        }
        // A user stop ends a sidecar with no evidence at all, and is never revived.
        rewrite(
            "unfinished-silent",
            serde_json::json!({"stoppedByUser": true, "revivedAt": at(1)}),
        );
        assert_eq!(poll(&path).running, base.running - 1);
    }

    #[test]
    fn task_stop_results_are_read_from_the_structured_result_or_the_content() {
        let json = |id: &str, kind: &str| {
            format!(
                r#"{{"message":"Successfully stopped task: {id} (Synthetic task)","task_id":"{id}","task_type":"{kind}","command":"Synthetic task"}}"#
            )
        };
        let entry = |content: serde_json::Value, error: bool| {
            let mut result = serde_json::json!({"type": "tool_result",
                "tool_use_id": "tool-stop", "content": content});
            if error {
                result["is_error"] = serde_json::json!(true);
            }
            serde_json::json!({"type": "user", "timestamp": TURN,
                "message": {"role": "user", "content": [result]}})
        };
        let text = |t: String| serde_json::json!([{"type": "text", "text": t}]);
        let agent = json("a", "local_agent");
        let mut structured = entry(serde_json::json!("Synthetic stop"), false);
        structured["toolUseResult"] = serde_json::from_str(&agent).unwrap();
        // A shell's stop in the structured result is not a subagent's, whatever the text.
        let mut shell = entry(serde_json::json!(agent), false);
        shell["toolUseResult"] = serde_json::from_str(&json("a", "local_bash")).unwrap();
        for (entry, id) in [
            (entry(serde_json::json!(agent), false), Some("a")),
            (entry(text(agent.clone()), false), Some("a")),
            (structured, Some("a")),
            (entry(serde_json::json!(agent), true), None),
            (entry(text(agent.clone()), true), None),
            (
                entry(serde_json::json!(json("a", "local_bash")), false),
                None,
            ),
            (
                entry(serde_json::json!("Synthetic: not JSON local_agent"), false),
                None,
            ),
            (shell, None),
        ] {
            assert_eq!(stopped(&entry).as_deref(), id, "{entry}");
            // The prefilter passes the escaped string form too.
            assert!(
                id.is_none() || Lifecycle::candidate(&entry.to_string()),
                "{entry}"
            );
        }
    }

    #[test]
    fn both_notification_forms_and_content_shapes_read_alike() {
        let text = |note: &str| {
            format!(
                "<task-notification>\n<task-id>a</task-id>\n<status>completed</status>\n\
                 <summary>Synthetic</summary>\n<note>{note}</note>\n</task-notification>"
            )
        };
        for (note, running) in [
            (
                "Synthetic: background work of its own still running.",
                Some(true),
            ),
            (
                "Synthetic: stops with no live background children.",
                Some(false),
            ),
            ("Synthetic: a note Fleet does not recognise.", Some(false)),
        ] {
            let text = text(note);
            let blocks = serde_json::json!([{"type": "text", "text": text}]);
            for content in [serde_json::json!(text), blocks] {
                let user = serde_json::json!({"type": "user",
                    "origin": {"kind": "task-notification"}, "message": {"content": content}});
                let queued = serde_json::json!({"type": "attachment", "attachment": {
                    "type": "queued_command", "commandMode": "task-notification",
                    "prompt": content}});
                for entry in [user, queued] {
                    assert_eq!(evidence(&entry), [("a".to_owned(), running)], "{entry}");
                }
            }
        }
        // Every terminal status ends the subagent unless its note says it runs on; a status
        // Fleet doesn't know leaves it to the sidecar.
        let notice = |status: &str, note: &str| {
            serde_json::json!({"type": "user", "origin": {"kind": "task-notification"},
                "message": {"content": format!("<task-notification><task-id>a</task-id>\
                    <status>{status}</status><note>{note}</note></task-notification>")}})
        };
        let interim = "background work of its own still running";
        for (status, note, running) in [
            ("killed", "", Some(false)),
            ("stopped", "", Some(false)),
            ("failed", "", Some(false)),
            ("failed", interim, Some(false)),
            ("killed", "new input arrived after this run", Some(true)),
            ("synthetic-unknown", "", None),
            ("synthetic-unknown", interim, None),
        ] {
            assert_eq!(
                evidence(&notice(status, note)),
                [("a".to_owned(), running)],
                "{status} {note}"
            );
        }
        // A user entry without the task-notification origin is not one, nor is a
        // notification that doesn't open the text or carries no id.
        for entry in [
            serde_json::json!({"type": "user", "origin": {"kind": "human"},
                "message": {"content": text("background work of its own still running")}}),
            serde_json::json!({"type": "user", "origin": {"kind": "task-notification"},
                "message": {"content": format!("Quoted: {}", text("x"))}}),
            serde_json::json!({"type": "user", "origin": {"kind": "task-notification"},
                "message": {"content": "<task-notification><status>completed</status></task-notification>"}}),
        ] {
            assert!(evidence(&entry).is_empty(), "{entry}");
        }
    }

    #[test]
    fn only_the_direct_children_before_result_are_read() {
        let n = notifications(
            "<task-notification><task-id>a</task-id><status>completed</status>\
             <summary>x <note>arrived after this run</note></summary>\
             <result><note>background work of its own still running</note></result>\
             <note>late</note></task-notification>",
        )
        .next()
        .unwrap();
        assert_eq!(
            (n.task_id, n.status, n.note),
            (Some("a"), Some("completed"), None)
        );
    }

    #[test]
    fn every_joined_notification_is_read_and_the_later_one_wins() {
        let element = |id: &str, note: &str| {
            format!(
                "<task-notification>\n<task-id>{id}</task-id>\n<status>completed</status>\n\
                 <summary>Synthetic</summary>\n<note>{note}</note>\n\
                 <result>Synthetic result</result>\n</task-notification>"
            )
        };
        let interim = "Synthetic: background work of its own still running.";
        let settled = "Synthetic: stops with no live background children.";
        let joined = [
            element("a", interim),
            element("b", interim),
            element("a", settled),
        ]
        .join("\n\n");
        let entry = serde_json::json!({"type": "user", "timestamp": TURN,
            "origin": {"kind": "task-notification"}, "message": {"content": joined}});
        assert_eq!(
            evidence(&entry),
            [
                ("a".to_owned(), Some(true)),
                ("b".to_owned(), Some(true)),
                ("a".to_owned(), Some(false))
            ]
        );
        let mut lifecycle = Lifecycle::default();
        lifecycle.observe(&entry);
        assert_eq!(
            Lifecycle::latest(&lifecycle, None, "a").unwrap().running,
            Some(false)
        );
        assert_eq!(
            Lifecycle::latest(&lifecycle, None, "b").unwrap().running,
            Some(true)
        );
        // Each element's <result> stays opaque: markup it quotes without the joiner is not
        // an element, and a <note> after it is not read.
        let quoted = format!(
            "<task-notification><task-id>c</task-id><status>completed</status>\
             <result>{} <note>{interim}</note></result></task-notification>\n\n{}",
            element("c", interim),
            element("d", settled)
        );
        let read: Vec<_> = notifications(&quoted)
            .map(|n| (n.task_id, n.note))
            .collect();
        assert_eq!(read, [(Some("c"), None), (Some("d"), Some(settled))]);
        // Any whitespace between a closing and an opening tag joins them, not only `\n\n`.
        for sep in ["\n", " ", "\r\n\t", ""] {
            let text = format!("{}{sep}{}", element("e", interim), element("f", settled));
            let ids: Vec<_> = notifications(&text).map(|n| n.task_id).collect();
            assert_eq!(ids, [Some("e"), Some("f")], "joined by {sep:?}");
        }
    }

    #[test]
    fn nested_owners_are_read_once_per_poll_and_never_outside_the_folder() {
        let (_dir, path) = lifecycle_fixture();
        let agents = path.with_extension("").join("subagents");
        OPENED.with(|opened| opened.borrow_mut().clear());
        poll(&path);
        let opened = OPENED.with(|opened| opened.take());
        let owner = agents.join("agent-owner.jsonl");
        // Four nested subagents share the owner; it is running, so no first-line read.
        assert_eq!(opened.iter().filter(|p| **p == owner).count(), 1);
        // An owner id that would leave the folder is never opened.
        fs::write(
            agents.join("agent-nested.meta.json"),
            r#"{"finished":true,"parentAgentId":"../escape"}"#,
        )
        .unwrap();
        for id in ["nested-settled", "nested-joined", "nested-joined-late"] {
            fs::remove_file(agents.join(format!("agent-{id}.meta.json"))).unwrap();
        }
        OPENED.with(|opened| opened.borrow_mut().clear());
        // Without its owner's interim note, nested is settled by the parent's note.
        assert_eq!(
            poll(&path),
            Counts {
                running: 9,
                finished: Some(9)
            }
        );
        let opened = OPENED.with(|opened| opened.take());
        assert!(opened.iter().all(|p| p.starts_with(&agents)), "{opened:?}");
        assert!(!opened.contains(&owner));
    }

    #[test]
    fn without_current_metas_no_notification_is_parsed_or_owner_read() {
        let (_dir, path) = lifecycle_fixture();
        let agents = path.with_extension("").join("subagents");
        let stale = UNIX_EPOCH + Duration::from_millis(START as u64) - Duration::from_secs(1);
        let metas: Vec<_> = fs::read_dir(&agents)
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .filter(|p| p.to_str().unwrap().ends_with(".meta.json"))
            .collect();
        for meta in &metas {
            File::options()
                .write(true)
                .open(meta)
                .unwrap()
                .set_times(FileTimes::new().set_modified(stale))
                .unwrap();
        }
        OBSERVED.with(|n| n.set(0));
        OPENED.with(|opened| opened.borrow_mut().clear());
        assert_eq!(
            poll(&path),
            Counts {
                running: 0,
                finished: Some(0)
            }
        );
        assert_eq!(OBSERVED.with(|n| n.get()), 0);
        assert!(OPENED.with(|opened| opened.take()).is_empty());
        // The same Transcript is parsed once a current meta asks for it, finished or not.
        File::options()
            .write(true)
            .open(agents.join("agent-unfinished-silent.meta.json"))
            .unwrap()
            .set_times(
                FileTimes::new()
                    .set_modified(UNIX_EPOCH + Duration::from_secs((START / 1000 + 120) as u64)),
            )
            .unwrap();
        assert_eq!(
            poll(&path),
            Counts {
                running: 1,
                finished: Some(0)
            }
        );
        assert!(OBSERVED.with(|n| n.get()) > 0);
    }

    #[test]
    fn fifo_metas_and_logs_are_skipped_without_opening_them() {
        run_fifo_child("static");
    }

    #[test]
    fn regular_files_replaced_with_fifos_after_stat_do_not_block_polls() {
        run_fifo_child("swap-meta");
        run_fifo_child("swap-log");
    }

    #[test]
    fn fifo_poll_child() {
        let Ok(case) = std::env::var("FLEET_SUBAGENT_FIFO_CASE") else {
            return;
        };
        let path = PathBuf::from(std::env::var_os("FLEET_SUBAGENT_FIFO_PATH").unwrap());
        let agents = path.with_extension("").join("subagents");
        let mkfifo = |path: &Path| {
            assert!(
                std::process::Command::new("mkfifo")
                    .arg(path)
                    .status()
                    .unwrap()
                    .success()
            );
        };
        if case == "static" {
            let log = agents.join("agent-finished.jsonl");
            fs::remove_file(&log).unwrap();
            mkfifo(&log);
            mkfifo(&agents.join("agent-fifo.meta.json"));
            let directory = agents.join("agent-stopped.jsonl");
            fs::remove_file(&directory).unwrap();
            fs::create_dir(&directory).unwrap();
            fs::create_dir(agents.join("agent-directory.meta.json")).unwrap();
            let counts = scan(&path, Some("2.1.301"), Some(START))
                .unwrap()
                .counts(Some(TURN), &Lifecycle::default());
            let opened = OPENED.with(|opened| opened.take());
            assert!(
                opened
                    .iter()
                    .all(|path| fs::metadata(path).unwrap().is_file())
            );
            assert_eq!(
                counts,
                Counts {
                    running: 2,
                    finished: Some(1)
                }
            );
        } else {
            let meta = case == "swap-meta";
            assert!(meta || case == "swap-log");
            let target = agents.join(if meta {
                "agent-running.meta.json"
            } else {
                "agent-finished.jsonl"
            });
            assert!(fs::metadata(&target).unwrap().is_file());
            let fifo = agents.join("replacement.fifo");
            mkfifo(&fifo);
            // The hook runs inside open(), after the poll's regular-file stat.
            REPLACE_BEFORE_OPEN
                .with(|replacement| *replacement.borrow_mut() = Some((target.clone(), fifo)));
            let counts = scan(&path, Some("2.1.301"), Some(START))
                .unwrap()
                .counts(Some(TURN), &Lifecycle::default());
            assert!(REPLACE_BEFORE_OPEN.with(|replacement| replacement.borrow().is_none()));
            assert_eq!(
                counts,
                Counts {
                    running: if meta { 1 } else { 2 },
                    finished: Some(if meta { 3 } else { 2 })
                }
            );
            // A nonblocking open alone is insufficient: the descriptor must be
            // rejected before any read, even if the FIFO would immediately EOF.
            assert!(open(&target).is_err());
        }
    }

    #[test]
    fn long_valid_descriptions_and_the_one_mib_boundary_count() {
        let (_dir, path) = fixture();
        let meta = path
            .with_extension("")
            .join("subagents/agent-long.meta.json");
        let long = serde_json::json!({"description": "x".repeat(100 * 1024)}).to_string();
        for text in [long, format!("{{}}{}", " ".repeat(1024 * 1024 - 2))] {
            fs::write(&meta, text).unwrap();
            assert_eq!(
                scan(&path, Some("2.1.301"), Some(START))
                    .unwrap()
                    .counts(Some(TURN), &Lifecycle::default()),
                Counts {
                    running: 3,
                    finished: Some(3)
                }
            );
        }
        // One byte beyond the cap, even if the JSON itself is valid.
        fs::write(&meta, format!("{{}}{}", " ".repeat(1024 * 1024 - 1))).unwrap();
        assert_eq!(
            scan(&path, Some("2.1.301"), Some(START))
                .unwrap()
                .counts(Some(TURN), &Lifecycle::default())
                .running,
            2
        );
    }

    #[test]
    fn oversized_and_malformed_metas_do_not_hide_valid_candidates() {
        let (_dir, path) = fixture();
        let agents = path.with_extension("").join("subagents");
        fs::write(
            agents.join("agent-large.meta.json"),
            format!("{{\"description\":\"{}\"}}", "x".repeat(1024 * 1024)),
        )
        .unwrap();
        fs::write(
            agents.join("agent-malformed.meta.json"),
            "{".repeat(1024 * 1024 + 1),
        )
        .unwrap();
        OPENED.with(|opened| opened.borrow_mut().clear());
        assert_eq!(
            scan(&path, Some("2.1.301"), Some(START))
                .unwrap()
                .counts(Some(TURN), &Lifecycle::default()),
            Counts {
                running: 2,
                finished: Some(3)
            }
        );
        let opened = OPENED.with(|opened| opened.take());
        assert!(!opened.contains(&agents.join("agent-large.meta.json")));
        assert!(!opened.contains(&agents.join("agent-malformed.meta.json")));
    }

    #[test]
    fn each_poll_opens_eligible_metas_and_only_current_finished_logs() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("large.jsonl");
        fs::write(&path, "").unwrap();
        let agents = dir.path().join("large/subagents");
        fs::create_dir_all(&agents).unwrap();
        let start = UNIX_EPOCH + Duration::from_millis(START as u64);
        let turn = timestamp(TURN).unwrap();
        for id in 0..210 {
            let meta = agents.join(format!("agent-{id}.meta.json"));
            fs::write(&meta, r#"{"finished":true}"#).unwrap();
            File::options()
                .write(true)
                .open(&meta)
                .unwrap()
                .set_times(FileTimes::new().set_modified(if id == 0 {
                    start - Duration::from_secs(1)
                } else if id < 200 {
                    turn - Duration::from_secs(1)
                } else {
                    turn
                }))
                .unwrap();
            fs::write(
                agents.join(format!("agent-{id}.jsonl")),
                format!("{{\"timestamp\":\"{TURN}\"}}\n"),
            )
            .unwrap();
        }
        let mut expected: Vec<_> = (1..210)
            .map(|id| agents.join(format!("agent-{id}.meta.json")))
            .chain((200..210).map(|id| agents.join(format!("agent-{id}.jsonl"))))
            .collect();
        expected.sort();
        for _ in 0..2 {
            OPENED.with(|opened| opened.borrow_mut().clear());
            assert_eq!(
                scan(&path, Some("2.1.301"), Some(START))
                    .unwrap()
                    .counts(Some(TURN), &Lifecycle::default()),
                Counts {
                    running: 0,
                    finished: Some(10)
                }
            );
            let mut opened = OPENED.with(|opened| opened.take());
            opened.sort();
            // Every fresh poll reads eligible metas once. Stale metas, old finished
            // logs and anything beyond the first line must never add more opens.
            assert_eq!(opened, expected);
        }
    }
}
