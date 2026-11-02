//! Subagent counts, and the subagents they count ([`Subagents`]): Claude Code's sidecars
//! ([`scan`]) and Codex's child rollouts ([`codex_report`]). pi is not supported yet.

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

/// `subagents` in `fleet list --json`: the counts, and the subagents they count.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Subagents {
    #[serde(flatten)]
    pub counts: Counts,
    /// At most [`MAX_ITEMS`]: the running ones, oldest start first, then the ones that
    /// ended in the Turn (the ones `finished` counts), latest end first.
    #[serde(default)]
    pub items: Vec<Item>,
}

/// How many subagents `items` lists at most.
pub const MAX_ITEMS: usize = 20;
/// How long a listed subagent's words may be, in characters.
const MAX_WORDS: usize = 500;
/// How much of a listed subagent's log is read for its last tool call.
const ACTIVITY_TAIL: u64 = 64 * 1024;

/// One subagent in `items`. Anything unknown is `None`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Item {
    /// Claude Code's agent id; Codex's child thread id.
    pub id: String,
    /// The subagent that started it; `None` for one the session itself started.
    pub parent: Option<String>,
    /// 1 for one the session started, 2 for one a subagent started, …
    pub depth: Option<u64>,
    pub state: State,
    /// What it was asked, short: Claude Code's description, the last part of Codex's
    /// `agent_path`.
    pub task: Option<String>,
    /// Claude Code's agent type (`general-purpose`, `Explore`, …).
    #[serde(rename = "type")]
    pub agent_type: Option<String>,
    /// Codex's nickname for the child (`Bohr`).
    pub name: Option<String>,
    /// Codex's role for the child.
    pub role: Option<String>,
    pub model: Option<String>,
    /// RFC 3339 UTC. Codex: its latest turn's.
    pub started_at: Option<String>,
    /// RFC 3339 UTC; `None` while it runs.
    pub ended_at: Option<String>,
    /// Its last tool call, short: `Bash: run the tests`.
    pub activity: Option<String>,
}

/// Running, or how it ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum State {
    Running,
    Completed,
    Failed,
    Stopped,
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
    description: Option<String>,
    agent_type: Option<String>,
    model: Option<String>,
    /// `spawnDepth`.
    depth: Option<u64>,
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
    description: Option<serde_json::Value>,
    #[serde(rename = "agentType")]
    agent_type: Option<serde_json::Value>,
    model: Option<serde_json::Value>,
    #[serde(rename = "spawnDepth")]
    spawn_depth: Option<serde_json::Value>,
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
            description: words(meta.description.as_ref()),
            agent_type: words(meta.agent_type.as_ref()),
            model: words(meta.model.as_ref()),
            depth: meta
                .spawn_depth
                .as_ref()
                .and_then(serde_json::Value::as_u64),
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
    #[cfg(test)]
    pub(crate) fn counts(self, turn_start: Option<&str>, lifecycle: &Lifecycle) -> Counts {
        self.report(turn_start, lifecycle, false).counts
    }

    /// [`Self::counts`], and with `items` the subagents counted. Listing reads more: each
    /// running one's first line (its start, to order them) and each listed one's tail.
    pub(crate) fn report(
        self,
        turn_start: Option<&str>,
        lifecycle: &Lifecycle,
        items: bool,
    ) -> Subagents {
        let mut owners: HashMap<String, Lifecycle> = HashMap::new();
        let mut running = Vec::new();
        // `(end, how, sidecar)` of each sidecar that ended.
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
                running.push(s);
                continue;
            }
            // A finished run ended at its meta's last write. An unfinished one ended with
            // the evidence that settled it.
            let end = match latest.and_then(|l| l.at) {
                Some(at) if !s.finished => at,
                _ => s.mtime,
            };
            // A user stop wins: real metas carry it beside `finished: true`.
            let how = if s.stopped_by_user {
                State::Stopped
            } else {
                latest.and_then(|l| l.ended).unwrap_or(State::Completed)
            };
            settled.push((end, how, s));
        }
        let turn = turn_start.and_then(timestamp);
        // `(start, end, how, sidecar)` of the ones that ended in the Turn.
        let mut done = Vec::new();
        let finished = if settled.is_empty() {
            Some(0)
        } else {
            turn.map(|turn| {
                for (end, how, s) in settled {
                    if end < turn {
                        continue;
                    }
                    if let Some(start) = first_timestamp(&s.log).filter(|start| *start >= turn) {
                        done.push((start, end, how, s));
                    }
                }
                done.len() as u64
            })
        };
        let counts = Counts {
            running: running.len() as u64,
            finished,
        };
        if !items {
            return Subagents {
                counts,
                items: Vec::new(),
            };
        }
        let mut running: Vec<_> = running
            .into_iter()
            .map(|s| (first_timestamp(&s.log), s))
            .collect();
        running.sort_by(|(a, s), (b, t)| (a.is_none(), a, &s.id).cmp(&(b.is_none(), b, &t.id)));
        done.sort_by(|(_, a, _, s), (_, b, _, t)| (b, &s.id).cmp(&(a, &t.id)));
        let items = running
            .into_iter()
            .map(|(start, s)| (start, None, State::Running, s))
            .chain(
                done.into_iter()
                    .map(|(start, end, how, s)| (Some(start), Some(end), how, s)),
            )
            .take(MAX_ITEMS)
            .map(|(start, end, state, s)| Item {
                activity: tail(&s.log, ACTIVITY_TAIL).and_then(|(text, _)| claude_activity(&text)),
                id: s.id,
                parent: s.owner,
                depth: s.depth,
                state,
                task: s.description,
                agent_type: s.agent_type,
                name: None,
                role: None,
                model: s.model,
                started_at: start.map(rfc3339),
                ended_at: end.map(rfc3339),
            })
            .collect();
        Subagents { counts, items }
    }
}

/// The last tool call in a Claude Code log's `text`: an assistant entry's last `tool_use`.
fn claude_activity(text: &str) -> Option<String> {
    text.lines().rev().find_map(|line| {
        if !line.contains("\"tool_use\"") {
            return None;
        }
        let v: serde_json::Value = serde_json::from_str(line).ok()?;
        let b = v["message"]["content"]
            .as_array()?
            .iter()
            .rev()
            .find(|b| b["type"] == "tool_use")?;
        Some(super::background::tool_words(
            b["name"].as_str()?,
            &b["input"],
        ))
    })
}

/// How much of a Codex child's rollout is read for its last turn event. A turn that ended
/// wrote its end last, so only one still running can start before this.
const CODEX_CHILD_TAIL: u64 = 256 * 1024;

/// A Codex child's last turn.
#[derive(Debug, Clone, Copy, PartialEq)]
struct ChildTurn {
    /// Its last turn event is `task_started`, or a cut tail holds none: a turn that has
    /// written more than the tail since it started.
    running: bool,
    /// When that turn started: its `task_started`, or, once that has left the tail, the
    /// `started_at` its end carries (whole seconds).
    started: Option<SystemTime>,
    /// When it ended, and how: `task_complete` completed, `turn_aborted` stopped.
    ended: Option<(Option<SystemTime>, State)>,
}

/// [`codex_report`]'s counts alone.
#[cfg(test)]
pub(crate) fn codex(children: &[&Path], turn_start: Option<&str>) -> Counts {
    codex_report(children, turn_start, None, false).counts
}

/// A Codex session's subagents from `children`, the rollouts of its descendant threads
/// (`thread_spawn.parent_thread_id`, at any depth) that its process wrote. A child runs
/// while its last turn does; one open between turns runs nothing, as the session's own Idle
/// says. `finished`: the ones not running whose last turn started at or after `turn_start`,
/// the last turn you started; `None` when that isn't known and one has ended. With `items`,
/// the subagents counted too, each listed one's first line read for who it is; `session`
/// is the session's own thread id, the parent of the children it started.
pub(crate) fn codex_report(
    children: &[&Path],
    turn_start: Option<&str>,
    session: Option<&str>,
    items: bool,
) -> Subagents {
    let turn = turn_start.and_then(timestamp);
    let mut running = Vec::new();
    let mut settled = 0;
    let mut done = Vec::new();
    for path in children {
        let Some((text, cut)) = tail(path, CODEX_CHILD_TAIL) else {
            continue;
        };
        let child = child_turn(&text, cut);
        let words = || {
            if items {
                child_words(&text)
            } else {
                Default::default()
            }
        };
        if child.running {
            running.push((child, *path, words()));
        } else {
            settled += 1;
            if turn.is_some_and(|turn| child.started.is_some_and(|s| s >= turn)) {
                done.push((child, *path, words()));
            }
        }
    }
    let counts = Counts {
        running: running.len() as u64,
        finished: if settled == 0 {
            Some(0)
        } else {
            turn.map(|_| done.len() as u64)
        },
    };
    if !items {
        return Subagents {
            counts,
            items: Vec::new(),
        };
    }
    // Ties, and children without a time, in path order: the same order every poll.
    running.sort_by_key(|(c, path, _)| (c.started.is_none(), c.started, *path));
    done.sort_by(|(a, p, _), (b, q, _)| {
        let end = |c: &ChildTurn| c.ended.and_then(|(at, _)| at);
        (end(b), p).cmp(&(end(a), q))
    });
    let items = running
        .into_iter()
        .chain(done)
        .take(MAX_ITEMS)
        .map(|(child, path, (model, activity))| {
            // A first line Fleet can't read leaves the child listed by its file's id, so the
            // list still holds every child the counts count.
            let head = child_head(path).unwrap_or_default();
            let spawn = &head["source"]["subagent"]["thread_spawn"];
            let field = |key: &str| words(spawn.get(key).or_else(|| head.get(key)));
            Item {
                id: words(head.get("id")).unwrap_or_else(|| rollout_id(path)),
                parent: field("parent_thread_id").filter(|p| Some(p.as_str()) != session),
                depth: spawn["depth"].as_u64(),
                state: child.ended.map_or(State::Running, |(_, how)| how),
                task: field("agent_path").map(|p| {
                    p.rsplit('/')
                        .find(|part| !part.is_empty())
                        .unwrap_or(&p)
                        .to_owned()
                }),
                agent_type: None,
                name: field("agent_nickname"),
                role: field("agent_role"),
                model,
                started_at: child.started.map(rfc3339),
                ended_at: child.ended.and_then(|(at, _)| at).map(rfc3339),
                activity,
            }
        })
        .collect();
    Subagents { counts, items }
}

/// The last turn in a child rollout's tail, `cut` when it starts mid-file. A child opens
/// with a zero-length copy of its parent's turn (`task_started`, then `turn_aborted`, in
/// one millisecond) before its own, so only the last turn event decides.
fn child_turn(text: &str, cut: bool) -> ChildTurn {
    let mut ended: Option<(Option<SystemTime>, State)> = None;
    let mut started_at = None;
    for line in text.lines().rev() {
        if !["task_started", "task_complete", "turn_aborted"]
            .iter()
            .any(|kind| line.contains(kind))
        {
            continue;
        }
        let Ok(v) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        if v["type"] != "event_msg" {
            continue;
        }
        let at = v["timestamp"].as_str().and_then(timestamp);
        match v["payload"]["type"].as_str() {
            Some("task_started") => {
                return ChildTurn {
                    running: ended.is_none(),
                    started: at,
                    ended,
                };
            }
            Some(kind @ ("task_complete" | "turn_aborted")) if ended.is_none() => {
                let how = if kind == "turn_aborted" {
                    State::Stopped
                } else {
                    State::Completed
                };
                ended = Some((at, how));
                let secs = v["payload"]["started_at"].as_u64();
                started_at = secs.map(|s| UNIX_EPOCH + Duration::from_secs(s));
            }
            _ => {}
        }
    }
    match ended {
        Some(_) => ChildTurn {
            running: false,
            started: started_at,
            ended,
        },
        // No turn event: a turn that outgrew the tail, or a child yet to start one.
        None => ChildTurn {
            running: cut,
            started: None,
            ended: None,
        },
    }
}

/// A Codex child's model and last tool call, from its rollout's tail: the last
/// `turn_context` or `thread_settings_applied`, and the last `function_call` or
/// `custom_tool_call`.
fn child_words(text: &str) -> (Option<String>, Option<String>) {
    let (mut model, mut activity) = (None, None);
    for line in text.lines().rev() {
        if model.is_some() && activity.is_some() {
            break;
        }
        let settings =
            line.contains("\"turn_context\"") || line.contains("thread_settings_applied");
        let call = line.contains("\"function_call\"") || line.contains("\"custom_tool_call\"");
        if !(settings && model.is_none() || call && activity.is_none()) {
            continue;
        }
        let Ok(v) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        let p = &v["payload"];
        if model.is_none() {
            model = match (v["type"].as_str(), p["type"].as_str()) {
                (Some("turn_context"), _) => words(p.get("model")),
                (Some("event_msg"), Some("thread_settings_applied")) => {
                    words(p["thread_settings"].get("model"))
                }
                _ => None,
            };
        }
        if activity.is_none() && v["type"] == "response_item" {
            activity = codex_call(p);
        }
    }
    (model, activity)
}

/// A Codex tool call in a few words. Codex 0.166 runs commands through `exec`, whose input
/// is JavaScript calling `tools.<name>({cmd: "…"})`: its first such call says what it does.
fn codex_call(p: &serde_json::Value) -> Option<String> {
    let name = p["name"].as_str()?;
    match p["type"].as_str()? {
        "function_call" => {
            let args = p["arguments"]
                .as_str()
                .and_then(|a| serde_json::from_str(a).ok());
            Some(super::background::tool_words(
                name,
                &args.unwrap_or(serde_json::Value::Null),
            ))
        }
        "custom_tool_call" => Some(
            p["input"]
                .as_str()
                .and_then(exec_call)
                .unwrap_or_else(|| name.to_owned()),
        ),
        _ => None,
    }
}

/// `tools.<name>(…)`'s name and its `cmd` or `command` string, in `exec`'s JavaScript: the
/// first such call outside a string literal, and the key inside that call's parentheses.
fn exec_call(code: &str) -> Option<String> {
    let ident = |c: char| c.is_ascii_alphanumeric() || c == '_' || c == '$';
    let mut chars = code.char_indices().peekable();
    let mut quote: Option<char> = None;
    let mut prev = ' ';
    // `(name, byte offset just inside its parenthesis)`.
    let mut call = None;
    while let Some((i, c)) = chars.next() {
        if let Some(q) = quote {
            if c == '\\' {
                chars.next();
            } else if c == q {
                quote = None;
            }
        } else if matches!(c, '"' | '\'' | '`') {
            quote = Some(c);
        } else if !ident(prev) && prev != '.' && code[i..].starts_with("tools.") {
            let rest = &code[i + "tools.".len()..];
            let len = rest.find(|c: char| !ident(c)).unwrap_or(rest.len());
            let after = rest[len..].trim_start();
            if len > 0 && after.starts_with('(') {
                let inside = code.len() - after.len() + 1;
                call = Some((&rest[..len], inside));
                break;
            }
        }
        prev = c;
    }
    let (name, inside) = call?;
    // The call's own arguments: up to its closing parenthesis, strings skipped.
    let mut depth = 1;
    let mut quote: Option<char> = None;
    let mut prev = '(';
    let mut what = None;
    let mut chars = code[inside..].char_indices();
    while let Some((i, c)) = chars.next() {
        if let Some(q) = quote {
            if c == '\\' {
                chars.next();
            } else if c == q {
                quote = None;
            }
            continue;
        }
        match c {
            '"' | '\'' | '`' => quote = Some(c),
            '(' | '{' | '[' => depth += 1,
            ')' | '}' | ']' => {
                depth -= 1;
                if depth == 0 {
                    break;
                }
            }
            _ if what.is_none() && !ident(prev) => {
                let rest = &code[inside + i..];
                what = ["cmd", "command"].iter().find_map(|key| {
                    let value = rest.strip_prefix(key)?.trim_start().strip_prefix(':')?;
                    js_string(value.trim_start())
                });
            }
            _ => {}
        }
        prev = c;
    }
    Some(super::background::call_words(name, what.as_deref()))
}

/// A rollout's thread id from its file name, `rollout-<time>-<id>.jsonl`: the trailing UUID,
/// or the whole stem when it doesn't end in one.
fn rollout_id(path: &Path) -> String {
    let stem = path
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    let uuid = stem
        .char_indices()
        .rev()
        .nth(35)
        .map(|(i, _)| &stem[i..])
        .filter(|tail| {
            tail.bytes().enumerate().all(|(i, b)| match i {
                8 | 13 | 18 | 23 => b == b'-',
                _ => b.is_ascii_hexdigit(),
            })
        });
    uuid.map_or_else(|| stem.clone(), str::to_owned)
}

/// The JavaScript string literal `text` starts with, its escapes undone.
fn js_string(text: &str) -> Option<String> {
    let mut chars = text.chars();
    let quote = chars.next().filter(|q| matches!(q, '"' | '\'' | '`'))?;
    let mut out = String::new();
    while let Some(c) = chars.next() {
        match c {
            '\\' => match chars.next()? {
                'n' => out.push('\n'),
                't' => out.push('\t'),
                other => out.push(other),
            },
            c if c == quote => return Some(out),
            c => out.push(c),
        }
    }
    None
}

/// A Codex child rollout's `session_meta` payload: its first line, at most 1 MiB.
fn child_head(path: &Path) -> Option<serde_json::Value> {
    let mut line = Vec::new();
    BufReader::new(open(path).ok()?)
        .take(1024 * 1024)
        .read_until(b'\n', &mut line)
        .ok()?;
    let mut v: serde_json::Value = serde_json::from_slice(&line).ok()?;
    (v["type"] == "session_meta").then(|| v["payload"].take())
}

/// The last `max` bytes of a regular file, from its first whole line, and whether that
/// cut anything off.
fn tail(path: &Path, max: u64) -> Option<(String, bool)> {
    if !fs::metadata(path).is_ok_and(|m| m.is_file()) {
        return None;
    }
    let mut file = open(path).ok()?;
    let size = file.metadata().ok()?.len();
    let start = size.saturating_sub(max);
    let mut buf = Vec::new();
    file.seek(SeekFrom::Start(start)).ok()?;
    file.take(max).read_to_end(&mut buf).ok()?;
    let text = String::from_utf8_lossy(&buf);
    Some(if start > 0 {
        // The first line is almost certainly cut in half.
        (
            text[text.find('\n').map_or(text.len(), |i| i + 1)..].to_owned(),
            true,
        )
    } else {
        (text.into_owned(), false)
    })
}

/// The latest lifecycle evidence per subagent id in one transcript tail: its task
/// notifications and successful `TaskStop` results. Built by a reverse pass, so the first
/// decisive entry seen for an id is its latest.
#[derive(Debug, Default, Clone)]
pub(crate) struct Lifecycle(HashMap<String, Latest>);

#[derive(Debug, Clone, Copy)]
struct Latest {
    at: Option<SystemTime>,
    /// How it ended, when `running` is `Some(false)`.
    ended: Option<State>,
    /// `Some(true)`: the run ended but the subagent did not (an interim or pending-input
    /// note). `Some(false)`: it ended (a terminal status or a successful `TaskStop`).
    /// `None`: a status Fleet doesn't know, which leaves it to the sidecar's `finished`.
    running: Option<bool>,
}

impl Lifecycle {
    /// Cheap prefilter for a reverse pass: only these lines are parsed for evidence.
    pub(crate) fn candidate(line: &str) -> bool {
        // Unquoted: a `TaskStop` or `SendMessage` result may carry it only inside an escaped
        // JSON string.
        line.contains("task-notification")
            || line.contains("local_agent")
            || line.contains("resumedAgentId")
            || line.contains("revivedAgentId")
    }

    /// Record `entry` unless a later one (seen earlier in the reverse pass) decided its id.
    /// Within the entry, a later notification for an id beats an earlier one.
    pub(crate) fn observe(&mut self, entry: &serde_json::Value) {
        #[cfg(test)]
        tests::OBSERVED.with(|n| n.set(n.get() + 1));
        let at = entry["timestamp"].as_str().and_then(timestamp);
        for (id, running, ended) in evidence_ended(entry).into_iter().rev() {
            self.0.entry(id).or_insert(Latest { at, ended, running });
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
/// notification with a `<status>` in `v`, or for a successful `TaskStop` (ended) or
/// `SendMessage` revive (running) of a subagent.
/// Anything else, routing notices included, decides nothing.
#[cfg(test)]
fn evidence(v: &serde_json::Value) -> Vec<(String, Option<bool>)> {
    evidence_ended(v)
        .into_iter()
        .map(|(id, running, _)| (id, running))
        .collect()
}

/// [`evidence`], with how an ended one ended: a notification's `failed` failed, `killed`
/// and `stopped` (and a `TaskStop`) stopped, `completed` completed.
fn evidence_ended(v: &serde_json::Value) -> Vec<(String, Option<bool>, Option<State>)> {
    let attachment = &v["attachment"];
    let content = match v["type"].as_str() {
        Some("user") if v["origin"]["kind"] == "task-notification" => &v["message"]["content"],
        Some("attachment")
            if attachment["type"] == "queued_command"
                && attachment["commandMode"] == "task-notification" =>
        {
            &attachment["prompt"]
        }
        Some("user") => {
            let results = tool_results(v);
            return results
                .iter()
                .find_map(stopped)
                .map(|id| (id, Some(false), Some(State::Stopped)))
                .into_iter()
                .chain(
                    results
                        .iter()
                        .find_map(revived)
                        .map(|id| (id, Some(true), None)),
                )
                .collect();
        }
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
            let ended = match status {
                _ if running != Some(false) => None,
                "failed" => Some(State::Failed),
                "killed" | "stopped" => Some(State::Stopped),
                _ => Some(State::Completed),
            };
            Some((n.task_id?.to_owned(), running, ended))
        })
        .collect()
}

/// The structured results of a tool call that is not an error: the entry's
/// `toolUseResult` when it has one, else each JSON in the `tool_result` content (a string,
/// or text blocks holding one).
fn tool_results(v: &serde_json::Value) -> Vec<serde_json::Value> {
    let Some(content) = v["message"]["content"].as_array() else {
        return Vec::new();
    };
    let results: Vec<_> = content
        .iter()
        .filter(|b| b["type"] == "tool_result")
        .collect();
    if results.is_empty() || results.iter().any(|b| b["is_error"] == true) {
        return Vec::new();
    }
    if v["toolUseResult"].is_object() {
        return vec![v["toolUseResult"].clone()];
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
        .filter_map(|text| serde_json::from_str(text).ok())
        .collect()
}

/// A subagent's id from a `TaskStop` result. `TaskStop` also stops shells, so the result
/// must name a `local_agent`.
fn stopped(result: &serde_json::Value) -> Option<String> {
    (result["task_type"] == "local_agent")
        .then(|| result["task_id"].as_str().map(str::to_owned))
        .flatten()
}

/// The subagent a `SendMessage` started a new run of. Claude Code 2.1.316 rewrites the
/// meta without `finished` and without `revivedAt` while that run goes, so the revive's
/// result is the only evidence newer than the earlier run's notification.
fn revived(result: &serde_json::Value) -> Option<String> {
    (result["success"] != false)
        .then(|| {
            result["revivedAgentId"]
                .as_str()
                .or_else(|| result["resumedAgentId"].as_str())
                .map(str::to_owned)
        })
        .flatten()
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
    let Some((text, _)) = tail(path, OWNER_TAIL) else {
        return lifecycle;
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

/// A subagent's start: its log's first entry's time, read within 1 MiB. A fork's log opens
/// with a `fork-context-ref` entry that has none, so the entry after it is the first; no
/// later line is ever searched for a more convenient time. Read for candidates that ended
/// in this Turn, and, when listing, for running ones (never to decide that one stopped).
fn first_timestamp(path: &Path) -> Option<SystemTime> {
    if !fs::metadata(path).ok()?.is_file() {
        return None;
    }
    let mut reader = BufReader::new(open(path).ok()?).take(1024 * 1024);
    for _ in 0..2 {
        let mut line = Vec::new();
        reader.read_until(b'\n', &mut line).ok()?;
        let v: serde_json::Value = serde_json::from_slice(&line).ok()?;
        if v["type"] != "fork-context-ref" {
            return timestamp(v["timestamp"].as_str()?);
        }
    }
    None
}

/// A short text field: trimmed, at most [`MAX_WORDS`] characters, `None` when empty.
fn words(v: Option<&serde_json::Value>) -> Option<String> {
    let text = v?.as_str()?.trim();
    if text.is_empty() {
        return None;
    }
    Some(if text.chars().count() > MAX_WORDS {
        text.chars().take(MAX_WORDS - 1).collect::<String>() + "…"
    } else {
        text.to_owned()
    })
}

fn rfc3339(t: SystemTime) -> String {
    chrono::DateTime::<chrono::Utc>::from(t).to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
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

    fn ids(s: &Subagents) -> Vec<(&str, State)> {
        s.items.iter().map(|i| (i.id.as_str(), i.state)).collect()
    }

    #[test]
    fn items_list_the_counted_sidecars_running_first_then_the_turns_latest_end() {
        let (_dir, path) = fixture();
        let agents = path.with_extension("").join("subagents");
        // A fork's log opens with an entry that has no time; the one after it is its start.
        fs::write(
            agents.join("agent-fork.jsonl"),
            "{\"type\":\"fork-context-ref\",\"ref\":\"x\"}\n{\"type\":\"user\",\"timestamp\":\"2026-01-01T00:01:10Z\"}\n",
        )
        .unwrap();
        // The last tool call, from the end of the log.
        fs::write(
            agents.join("agent-finished.jsonl"),
            [
                r#"{"type":"user","timestamp":"2026-01-01T00:01:10Z"}"#,
                r#"{"type":"assistant","message":{"content":[{"type":"tool_use","name":"Read","input":{"file_path":"/work/a.rs"}}]}}"#,
                r#"{"type":"assistant","message":{"content":[{"type":"text","text":"x"},{"type":"tool_use","name":"Bash","input":{"command":"cargo test\nmore","description":"Run the tests"}}]}}"#,
                r#"{"type":"user","message":{"content":[{"type":"tool_result","content":"ok"}]}}"#,
            ]
            .join("\n")
                + "\n",
        )
        .unwrap();
        let set = |name: &str, seconds: u64, text: Option<&str>| {
            let meta = agents.join(format!("agent-{name}.meta.json"));
            if let Some(text) = text {
                fs::write(&meta, text).unwrap();
            }
            File::options()
                .write(true)
                .open(meta)
                .unwrap()
                .set_times(FileTimes::new().set_modified(
                    UNIX_EPOCH + Duration::from_secs((START / 1000) as u64 + seconds),
                ))
                .unwrap();
        };
        // A user stop wins over `finished: true`; the latest end comes first.
        set(
            "stopped",
            150,
            Some(
                r#"{"agentType":"Explore","description":"Look around","model":"claude-haiku-5-5","spawnDepth":1,"finished":true,"stoppedByUser":true}"#,
            ),
        );
        set("fork", 130, None);
        let mut lifecycle = Lifecycle::default();
        lifecycle.observe(&serde_json::json!({"type": "user",
            "origin": {"kind": "task-notification"}, "timestamp": "2026-01-01T00:02:10Z",
            "message": {"content": "<task-notification><task-id>fork</task-id><status>failed</status></task-notification>"}}));
        let s = scan(&path, Some("2.1.301"), Some(START)).unwrap();
        let report = s.report(Some(TURN), &lifecycle, true);
        assert_eq!(
            report.counts,
            Counts {
                running: 2,
                finished: Some(3)
            }
        );
        // Running ones without a log have no start: last among the running, by id.
        assert_eq!(
            ids(&report),
            [
                ("nested", State::Running),
                ("running", State::Running),
                ("stopped", State::Stopped),
                ("fork", State::Failed),
                ("finished", State::Completed),
            ]
        );
        let item = |id: &str| report.items.iter().find(|i| i.id == id).unwrap();
        let nested = item("nested");
        assert_eq!(
            (
                nested.parent.as_deref(),
                nested.depth,
                nested.started_at.as_deref()
            ),
            (Some("running"), Some(2), None)
        );
        let stopped = item("stopped");
        assert_eq!(
            (
                stopped.task.as_deref(),
                stopped.agent_type.as_deref(),
                stopped.model.as_deref(),
                stopped.parent.as_deref(),
                stopped.depth
            ),
            (
                Some("Look around"),
                Some("Explore"),
                Some("claude-haiku-5-5"),
                None,
                Some(1)
            )
        );
        assert_eq!(
            stopped.ended_at.as_deref(),
            Some("2026-01-01T00:02:30.000Z")
        );
        assert_eq!(
            item("fork").started_at.as_deref(),
            Some("2026-01-01T00:01:10.000Z")
        );
        let finished = item("finished");
        assert_eq!(finished.activity.as_deref(), Some("Bash: Run the tests"));
        assert_eq!(
            (finished.name.as_ref(), finished.role.as_ref()),
            (None, None)
        );
        // Without the Turn, nothing ended can be placed, so only the running ones list.
        let s = scan(&path, Some("2.1.301"), Some(START)).unwrap();
        let report = s.report(None, &lifecycle, true);
        assert_eq!(report.counts.finished, None);
        assert_eq!(report.items.len(), 2);
        // Counting alone lists nothing.
        let s = scan(&path, Some("2.1.301"), Some(START)).unwrap();
        assert!(s.report(Some(TURN), &lifecycle, false).items.is_empty());
    }

    #[test]
    fn items_stop_at_twenty_keeping_the_running_ones_oldest_first() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("many.jsonl");
        fs::write(&path, "").unwrap();
        let agents = dir.path().join("many/subagents");
        fs::create_dir_all(&agents).unwrap();
        let turn = timestamp(TURN).unwrap();
        for id in 0..25 {
            let meta = agents.join(format!("agent-{id:02}.meta.json"));
            fs::write(&meta, format!(r#"{{"finished":{}}}"#, id >= 18)).unwrap();
            File::options()
                .write(true)
                .open(&meta)
                .unwrap()
                .set_times(FileTimes::new().set_modified(turn + Duration::from_secs(60 + id)))
                .unwrap();
            // Running ones start later the lower their id; finished ones in the Turn.
            let start = chrono::DateTime::<chrono::Utc>::from(turn + Duration::from_secs(50 - id))
                .to_rfc3339();
            fs::write(
                agents.join(format!("agent-{id:02}.jsonl")),
                format!("{{\"timestamp\":\"{start}\"}}\n"),
            )
            .unwrap();
        }
        let s = scan(&path, Some("2.1.301"), Some(START)).unwrap();
        let report = s.report(Some(TURN), &Lifecycle::default(), true);
        assert_eq!(
            report.counts,
            Counts {
                running: 18,
                finished: Some(7)
            }
        );
        let listed: Vec<&str> = report.items.iter().map(|i| i.id.as_str()).collect();
        let running: Vec<String> = (0..18).rev().map(|id| format!("{id:02}")).collect();
        assert_eq!(listed.len(), MAX_ITEMS);
        assert_eq!(
            listed[..18],
            running.iter().map(String::as_str).collect::<Vec<_>>()[..]
        );
        // The two latest ends of the seven that finished.
        assert_eq!(listed[18..], ["24", "23"]);
    }

    #[test]
    fn codex_items_say_who_each_child_is_from_its_latest_turn() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../testdata/subagents/codex");
        let path = |name: &str| root.join(format!("{name}.jsonl"));
        let dir = tempfile::tempdir().unwrap();
        // Started by `running`, its rollout opens with a copy of the parent's turn, aborted,
        // then ran two turns of its own: the second, still running, is the one that counts.
        let nested = dir.path().join("nested.jsonl");
        fs::write(
            &nested,
            [
                r#"{"type":"session_meta","payload":{"id":"nested","source":{"subagent":{"thread_spawn":{"parent_thread_id":"running","depth":2,"agent_path":"/root/review_diff/fix/"}}}}}"#,
                r#"{"type":"event_msg","timestamp":"2026-01-01T00:00:50Z","payload":{"type":"task_started"}}"#,
                r#"{"type":"event_msg","timestamp":"2026-01-01T00:00:50Z","payload":{"type":"turn_aborted"}}"#,
                r#"{"type":"event_msg","timestamp":"2026-01-01T00:01:10Z","payload":{"type":"task_started"}}"#,
                r#"{"type":"event_msg","timestamp":"2026-01-01T00:01:20Z","payload":{"type":"task_complete"}}"#,
                r#"{"type":"event_msg","timestamp":"2026-01-01T00:01:30Z","payload":{"type":"task_started"}}"#,
                r#"{"type":"response_item","payload":{"type":"function_call","name":"wait","arguments":"not json"}}"#,
            ]
            .join("\n")
                + "\n",
        )
        .unwrap();
        let aborted = dir.path().join("aborted.jsonl");
        fs::write(
            &aborted,
            [
                r#"{"type":"session_meta","payload":{"id":"aborted","source":{"subagent":{"thread_spawn":{"parent_thread_id":"parent","depth":1}}}}}"#,
                r#"{"type":"event_msg","timestamp":"2026-01-01T00:01:05Z","payload":{"type":"task_started"}}"#,
                r#"{"type":"event_msg","timestamp":"2026-01-01T00:02:30Z","payload":{"type":"turn_aborted"}}"#,
            ]
            .join("\n")
                + "\n",
        )
        .unwrap();
        let (running, finished, prior) = (path("running"), path("finished"), path("prior"));
        let all = [
            running.as_path(),
            finished.as_path(),
            prior.as_path(),
            nested.as_path(),
            aborted.as_path(),
        ];
        let report = codex_report(&all, Some(TURN), Some("parent"), true);
        assert_eq!(
            report.counts,
            Counts {
                running: 2,
                finished: Some(2)
            }
        );
        assert_eq!(
            ids(&report),
            [
                ("running", State::Running),
                ("nested", State::Running),
                ("aborted", State::Stopped),
                ("finished", State::Completed),
            ]
        );
        let item = |id: &str| report.items.iter().find(|i| i.id == id).unwrap();
        let r = item("running");
        assert_eq!(
            (
                r.parent.as_deref(),
                r.depth,
                r.task.as_deref(),
                r.name.as_deref(),
                r.role.as_deref(),
                r.model.as_deref(),
                r.started_at.as_deref(),
                r.ended_at.as_deref(),
                r.agent_type.as_deref(),
            ),
            (
                None,
                Some(1),
                Some("review_diff"),
                Some("Bohr"),
                Some("reviewer"),
                Some("gpt-6-luna"),
                Some("2026-01-01T00:01:00.000Z"),
                None,
                None,
            )
        );
        assert_eq!(
            r.activity.as_deref(),
            Some(r#"exec_command: pnpm test --filter "rates""#)
        );
        let f = item("finished");
        // The later settings event names the model; `function_call` arguments say what.
        assert_eq!(
            (
                f.model.as_deref(),
                f.activity.as_deref(),
                f.ended_at.as_deref(),
                f.role.as_deref()
            ),
            (
                Some("gpt-6-sol"),
                Some("spawn_agent: check_rates"),
                Some("2026-01-01T00:02:00.000Z"),
                None
            )
        );
        let n = item("nested");
        assert_eq!(
            (
                n.parent.as_deref(),
                n.depth,
                n.task.as_deref(),
                n.started_at.as_deref(),
                n.activity.as_deref()
            ),
            (
                Some("running"),
                Some(2),
                Some("fix"),
                Some("2026-01-01T00:01:30.000Z"),
                Some("wait")
            )
        );
        // Counting alone reads no child's first line and lists nothing.
        assert!(codex_report(&all, Some(TURN), None, false).items.is_empty());
        // A first line Fleet can't read still lists the child, by its file's id.
        let headless = dir
            .path()
            .join("rollout-2026-01-01T00-01-00-01a19bb2-27cb-7ab0-9e30-752427c678fd.jsonl");
        let text = fs::read_to_string(&running).unwrap();
        fs::write(&headless, text.replacen("session_meta", "other", 1)).unwrap();
        let report = codex_report(&[headless.as_path()], Some(TURN), None, true);
        assert_eq!(
            ids(&report),
            [("01a19bb2-27cb-7ab0-9e30-752427c678fd", State::Running)]
        );
        assert_eq!(report.items[0].name, None);
        assert_eq!(rollout_id(Path::new("/x/running.jsonl")), "running");
    }

    #[test]
    fn exec_calls_are_named_by_their_first_tool_and_its_command() {
        for (code, words) in [
            (
                "const r = await tools.exec_command({cmd:\"ls -la\",workdir:\"/w\"});",
                "exec_command: ls -la",
            ),
            (
                "await Promise.all([\n tools.exec_command({command: 'git status\\nmore'}),\n tools.exec_command({cmd:\"b\"})]);",
                "exec_command: git status",
            ),
            (
                "text(await tools.write_stdin({session_id: 3}));",
                "write_stdin",
            ),
            ("tools.view_image({path:`a.png`})", "view_image"),
        ] {
            assert_eq!(exec_call(code).as_deref(), Some(words), "{code}");
        }
        for (code, words) in [
            // `tools.` inside a string or another name is not a call; a command naming it
            // is read whole.
            (
                "const s = 'tools.x(1)'; mytools.y(); tools.exec_command({cmd: \"cat tools.rs\"})",
                "exec_command: cat tools.rs",
            ),
            // Only this call's own key: a later call's isn't its.
            (
                "tools.write_stdin({session_id: 3}); tools.exec_command({cmd:\"ls\"})",
                "write_stdin",
            ),
            (
                "tools.exec_command({ cmd : \"é ü 漢字\" })",
                "exec_command: é ü 漢字",
            ),
            ("tools.exec_command({cmd:\"unterminated", "exec_command"),
            ("tools.exec_command({mycmd:\"no\"})", "exec_command"),
        ] {
            assert_eq!(exec_call(code).as_deref(), Some(words), "{code}");
        }
        assert_eq!(exec_call("tools.exec_command"), None);
        assert_eq!(exec_call("text(1)"), None);
        assert_eq!(exec_call("tools.(x)"), None);
        assert_eq!(js_string("\"unterminated"), None);
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
        let (_, turn, lifecycle, _) =
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
    fn a_codex_childs_last_turn_event_decides_and_its_own_start_places_it() {
        let ev = |at: &str, kind: &str| {
            format!(r#"{{"timestamp":"{at}","type":"event_msg","payload":{{"type":"{kind}"}}}}"#)
        };
        let at = |t: &str| timestamp(t);
        // Spawned: its parent's turn, copied and aborted in one millisecond, then its own.
        let copy = [
            ev("2026-01-01T00:01:00.100Z", "task_started"),
            ev("2026-01-01T00:01:00.100Z", "turn_aborted"),
        ]
        .join("\n");
        let own = format!("{copy}\n{}", ev("2026-01-01T00:01:00.110Z", "task_started"));
        let ran = ChildTurn {
            running: true,
            started: at("2026-01-01T00:01:00.110Z"),
            ended: None,
        };
        assert_eq!(child_turn(&own, false), ran);
        for (end, how) in [
            ("task_complete", State::Completed),
            ("turn_aborted", State::Stopped),
        ] {
            let ended = format!("{own}\n{}", ev("2026-01-01T00:01:47Z", end));
            assert_eq!(
                child_turn(&ended, false),
                ChildTurn {
                    running: false,
                    ended: Some((at("2026-01-01T00:01:47Z"), how)),
                    ..ran
                },
                "{end}"
            );
            // Its start has left the tail: the end says when it started.
            let alone = format!(
                r#"{{"timestamp":"2026-01-01T00:01:47Z","type":"event_msg","payload":{{"type":"{end}","started_at":1767225660}}}}"#
            );
            assert_eq!(
                child_turn(&alone, true),
                ChildTurn {
                    running: false,
                    started: at("2026-01-01T00:01:00Z"),
                    ended: Some((at("2026-01-01T00:01:47Z"), how)),
                },
                "{end}"
            );
        }
        // Between its copy and its own turn, it runs nothing yet.
        assert!(!child_turn(&copy, false).running);
        // No turn event in a cut tail: a turn that outgrew it. A record merely naming one
        // isn't one, and a whole rollout without one hasn't started.
        let quiet =
            r#"{"type":"response_item","payload":{"type":"message","text":"task_complete"}}"#;
        assert_eq!(
            child_turn(quiet, true),
            ChildTurn {
                running: true,
                started: None,
                ended: None,
            }
        );
        assert!(!child_turn(quiet, false).running);
    }

    #[test]
    fn codex_counts_running_children_and_the_ones_that_ended_in_the_turn() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../testdata/subagents/codex");
        let path = |name: &str| root.join(format!("{name}.jsonl"));
        let (running, finished, prior) = (path("running"), path("finished"), path("prior"));
        let all = [running.as_path(), finished.as_path(), prior.as_path()];
        let turn = Some("2026-01-01T00:01:00Z");
        assert_eq!(
            codex(&all, turn),
            Counts {
                running: 1,
                finished: Some(1)
            }
        );
        // An ended child can't be placed without the Turn's start; a running one can.
        assert_eq!(
            codex(&all, None),
            Counts {
                running: 1,
                finished: None
            }
        );
        assert_eq!(
            codex(&[running.as_path()], None),
            Counts {
                running: 1,
                finished: Some(0)
            }
        );
        // One that wrote more than the tail in its turn still counts as done; one with no
        // turn yet doesn't run.
        let dir = tempfile::tempdir().unwrap();
        let big = dir.path().join("big.jsonl");
        let filler = format!(
            "{}\n",
            r#"{"type":"response_item","payload":{"type":"message","text":"x"}}"#
        );
        let mut text = fs::read_to_string(&finished).unwrap();
        let end = text.lines().last().unwrap().to_owned();
        text.truncate(text.len() - end.len() - 1);
        text += &filler.repeat(CODEX_CHILD_TAIL as usize / filler.len() + 1);
        text += &end.replace(
            r#""task_complete""#,
            r#""task_complete","started_at":1767225660"#,
        );
        fs::write(&big, text + "\n").unwrap();
        let unstarted = dir.path().join("unstarted.jsonl");
        let meta = fs::read_to_string(&running).unwrap();
        fs::write(&unstarted, meta.lines().next().unwrap()).unwrap();
        assert_eq!(
            codex(&[big.as_path(), unstarted.as_path()], turn),
            Counts {
                running: 0,
                finished: Some(1)
            }
        );
        // A rollout that's gone, or isn't a regular file, counts for nothing.
        assert_eq!(
            codex(&[path("missing").as_path(), root.as_path()], turn),
            Counts {
                running: 0,
                finished: Some(0)
            }
        );
    }

    #[test]
    fn a_send_message_revive_runs_a_finished_sidecar_until_its_next_notification() {
        let (_dir, path) = lifecycle_fixture();
        let base = poll(&path);
        // Claude Code 2.1.316 rewrites a revived subagent's meta without `finished` and
        // without `revivedAt` while the new run goes.
        let meta = path
            .with_extension("")
            .join("subagents/agent-settled.meta.json");
        fs::write(&meta, r#"{"spawnDepth":1}"#).unwrap();
        set_mtime(&meta, 200);
        // Alone, that reads as the earlier run's settled notice (00:01:20) ending it.
        assert_eq!(poll(&path), base);
        let append = |entry: serde_json::Value| {
            let mut file = File::options().append(true).open(&path).unwrap();
            std::io::Write::write_all(&mut file, format!("{entry}\n").as_bytes()).unwrap();
        };
        let revive = |at: &str, result: serde_json::Value, error: bool| {
            let mut entry = serde_json::json!({"type": "user", "timestamp": at, "message": {
                "role": "user", "content": [{"type": "tool_result", "tool_use_id": "tool-send",
                    "is_error": error, "content": [{"type": "text", "text": result.to_string()}]}]}});
            entry["toolUseResult"] = result;
            entry
        };
        let resumed = serde_json::json!({"success": true, "message": "Resuming agent settled",
            "resumedAgentId": "settled", "revivedAgentId": "settled"});
        // A failed send revives nothing.
        append(revive("2026-01-01T00:01:30Z", resumed.clone(), true));
        assert_eq!(poll(&path), base);
        append(revive(
            "2026-01-01T00:01:40Z",
            serde_json::json!({"success": false, "resumedAgentId": "settled"}),
            false,
        ));
        assert_eq!(poll(&path), base);
        append(revive("2026-01-01T00:01:50Z", resumed, false));
        let revived = Counts {
            running: base.running + 1,
            finished: base.finished.map(|n| n - 1),
        };
        assert_eq!(poll(&path), revived);
        // The revived run's own notice settles it again, finished in this Turn.
        append(
            serde_json::json!({"type": "user", "timestamp": "2026-01-01T00:01:55Z",
            "origin": {"kind": "task-notification"}, "message": {"role": "user", "content":
            "<task-notification>\n<task-id>settled</task-id>\n<status>completed</status>\n\
             <note>Synthetic note: it stops with no live background children.</note>\n\
             </task-notification>"}}),
        );
        assert_eq!(poll(&path), base);
    }

    #[test]
    fn send_message_revives_are_read_from_the_structured_result_or_the_content() {
        let entry = |content: serde_json::Value| {
            serde_json::json!({"type": "user", "timestamp": TURN, "message": {"role": "user",
                "content": [{"type": "tool_result", "tool_use_id": "tool-send",
                    "content": content}]}})
        };
        let both = r#"{"success":true,"resumedAgentId":"a","revivedAgentId":"a"}"#;
        let resumed = r#"{"success":true,"message":"Resuming agent a","resumedAgentId":"a"}"#;
        let mut structured = entry(serde_json::json!("Resuming agent a"));
        structured["toolUseResult"] = serde_json::from_str(both).unwrap();
        for (entry, id) in [
            (structured, Some("a")),
            (entry(serde_json::json!(resumed)), Some("a")),
            (
                entry(serde_json::json!([{"type": "text", "text": resumed}])),
                Some("a"),
            ),
            // A message to a running subagent is only queued.
            (
                entry(serde_json::json!(
                    r#"{"success":true,"message":"Message queued"}"#
                )),
                None,
            ),
            (
                entry(serde_json::json!(
                    r#"{"success":false,"resumedAgentId":"a","revivedAgentId":"a"}"#
                )),
                None,
            ),
        ] {
            assert_eq!(
                tool_results(&entry).iter().find_map(revived).as_deref(),
                id,
                "{entry}"
            );
            assert!(
                id.is_none() || Lifecycle::candidate(&entry.to_string()),
                "{entry}"
            );
        }
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
            assert_eq!(
                tool_results(&entry).iter().find_map(stopped).as_deref(),
                id,
                "{entry}"
            );
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
        // Listing adds one read of each listed log's end, and nothing else.
        let mut listed = expected.clone();
        listed.extend((200..210).map(|id| agents.join(format!("agent-{id}.jsonl"))));
        listed.sort();
        OPENED.with(|opened| opened.borrow_mut().clear());
        let report = scan(&path, Some("2.1.301"), Some(START)).unwrap().report(
            Some(TURN),
            &Lifecycle::default(),
            true,
        );
        assert_eq!(report.items.len(), 10);
        let mut opened = OPENED.with(|opened| opened.take());
        opened.sort();
        assert_eq!(opened, listed);
    }
}
